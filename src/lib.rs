use pgrx::heap_tuple::{AllocatedByRust, PgHeapTuple};
use pgrx::iter::SetOfIterator;
use pgrx::prelude::*;
use pgrx::tupdesc::PgTupleDesc;
use serde_json::Value as JsonValue;

use pgrx::JsonB;
use substrait::proto::plan::Plan;
use substrait::proto::rel::read_rel::ReadType;
use substrait::proto::rel::RelType;

pgrx::pg_module_magic!();

/// Column description for PG
#[derive(Debug, Clone)]
struct PgCol {
    name: String,
    oid: pg_sys::Oid,
    typmod: i32,
}

/// Map Substrait → PG type OID
fn substrait_type_to_pg(col_name: &str, substrait_typ: &substrait::proto::r#type::Kind) -> PgCol {
    use substrait::proto::r#type;
    match substrait_typ {
        r#type::Kind::Bool(_) => PgCol {
            name: col_name.into(),
            oid: pg_sys::BOOLOID,
            typmod: -1,
        },
        r#type::Kind::I32(_) => PgCol {
            name: col_name.into(),
            oid: pg_sys::INT4OID,
            typmod: -1,
        },
        r#type::Kind::I64(_) => PgCol {
            name: col_name.into(),
            oid: pg_sys::INT8OID,
            typmod: -1,
        },
        r#type::Kind::Fp32(_) => PgCol {
            name: col_name.into(),
            oid: pg_sys::FLOAT4OID,
            typmod: -1,
        },
        r#type::Kind::Fp64(_) => PgCol {
            name: col_name.into(),
            oid: pg_sys::FLOAT8OID,
            typmod: -1,
        },
        r#type::Kind::String(_) => PgCol {
            name: col_name.into(),
            oid: pg_sys::TEXTOID,
            typmod: -1,
        },
        r#type::Kind::Binary(_) => PgCol {
            name: col_name.into(),
            oid: pg_sys::BYTEAOID,
            typmod: -1,
        },
        r#type::Kind::Date(_) => PgCol {
            name: col_name.into(),
            oid: pg_sys::DATEOID,
            typmod: -1,
        },
        r#type::Kind::Timestamp(_) => PgCol {
            name: col_name.into(),
            oid: pg_sys::TIMESTAMPTZOID,
            typmod: -1,
        },
        _ => error!("Unsupported Substrait type for column '{}'", col_name),
    }
}

/// A normalized scan from Substrait
#[derive(Debug, Clone)]
struct ScanSpec {
    schema_name: Option<String>,
    table_name: String,
    cols: Vec<PgCol>, // projected/output cols
    projected_indices: Vec<usize>,
    base_names: Vec<String>, // all base col names
}

/// Extract scan info from Substrait Plan
fn extract_scan(plan: &Plan) -> ScanSpec {
    if plan.relations.is_empty() {
        error!("Substrait plan has no relations");
    }

    let rel = plan.relations[0]
        .rel
        .as_ref()
        .unwrap_or_else(|| error!("Relation missing 'rel'"));
    let input_rel = match rel {
        RelType::Root(root) => root
            .input
            .as_ref()
            .unwrap_or_else(|| error!("RootRel missing 'input'")),
        _ => error!("Top-level must be RootRel"),
    };
    let read = match &input_rel.rel_type {
        Some(RelType::Read(r)) => r,
        _ => error!("RootRel must contain ReadRel"),
    };

    // NamedTable
    let (schema_name, table_name) = match read.read_type.as_ref() {
        Some(ReadType::NamedTable(nt)) => {
            if nt.names.is_empty() {
                error!("NamedTable empty");
            }
            if nt.names.len() == 1 {
                (None, nt.names[0].clone())
            } else {
                let t = nt.names.last().unwrap().clone();
                let s = nt.names[..nt.names.len() - 1].join(".");
                (Some(s), t)
            }
        }
        _ => error!("Only NamedTable supported"),
    };

    // baseSchema
    let mut base_cols = Vec::new();
    let mut base_names = Vec::new();
    if let Some(base_schema) = &read.base_schema {
        let type_vec = base_schema
            .struct_
            .as_ref()
            .map(|s| &s.types)
            .unwrap_or_else(|| error!("baseSchema.struct missing"));
        for (i, t) in type_vec.iter().enumerate() {
            let name = base_schema
                .names
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("col{}", i + 1));
            let kind = t
                .kind
                .as_ref()
                .unwrap_or_else(|| error!("baseSchema.types[i].kind missing"));
            base_cols.push(substrait_type_to_pg(&name, kind));
            base_names.push(name);
        }
    } else {
        error!("ReadRel requires baseSchema");
    }

    // projection
    let mut projected_indices = Vec::new();
    if let Some(proj) = &read.projection {
        if let Some(sel) = &proj.select {
            for item in &sel.struct_items {
                let idx = item.field.unwrap_or(0) as usize;
                if idx >= base_cols.len() {
                    error!("Projection index {} out of range", idx);
                }
                projected_indices.push(idx);
            }
        }
    }

    let cols = if projected_indices.is_empty() {
        base_cols.clone()
    } else {
        projected_indices
            .iter()
            .map(|&i| base_cols[i].clone())
            .collect()
    };

    ScanSpec {
        schema_name,
        table_name,
        cols,
        projected_indices,
        base_names,
    }
}

/// Build SELECT SQL for SPI fallback
fn build_select_sql(scan: &ScanSpec) -> String {
    let qname = match &scan.schema_name {
        Some(s) => format!(
            "{}.{}",
            pgrx::name::escape_ident(s),
            pgrx::name::escape_ident(&scan.table_name)
        ),
        None => pgrx::name::escape_ident(&scan.table_name),
    };

    let cols_sql = if scan.projected_indices.is_empty() {
        scan.base_names
            .iter()
            .map(|n| pgrx::name::escape_ident(n))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        scan.projected_indices
            .iter()
            .map(|&i| pgrx::name::escape_ident(&scan.base_names[i]))
            .collect::<Vec<_>>()
            .join(", ")
    };

    format!("SELECT {} FROM {}", cols_sql, qname)
}

/// Build runtime TupleDesc
unsafe fn build_tupdesc(cols: &[PgCol]) -> PgTupleDesc<'static> {
    let tupdesc = pg_sys::CreateTemplateTupleDesc(cols.len() as i32);
    for (i, col) in cols.iter().enumerate() {
        let attname = std::ffi::CString::new(col.name.clone()).unwrap();
        pg_sys::TupleDescInitEntry(
            tupdesc,
            (i + 1) as i32,
            attname.as_ptr(),
            col.oid,
            col.typmod,
            0,
        );
    }
    PgTupleDesc::from_pg_copy(pg_sys::BlessTupleDesc(tupdesc))
}

/// Convert SPI row into a PgHeapTuple
unsafe fn row_to_heap_tuple(
    row: &pgrx::SpiHeapTuple,
    cols: &[PgCol],
    tupdesc: &PgTupleDesc<'static>,
) -> PgHeapTuple<'static, AllocatedByRust> {
    let datums = cols.iter().map(|col| {
        let (d_opt, _) = row.get_datum_by_name(&col.name).expect("col fetch");
        d_opt
    });
    PgHeapTuple::from_datums(tupdesc.clone(), datums).expect("tuple build")
}

/// -------------------------------
/// substrait_exec_raw (needs AS)
/// -------------------------------
#[pg_extern]
fn substrait_exec_raw(
    plan_json: &str,
) -> SetOfIterator<'static, PgHeapTuple<'static, AllocatedByRust>> {
    let plan: Plan = serde_json::from_str(plan_json)
        .map_err(|e| error!("{}", e))
        .unwrap();
    let scan = extract_scan(&plan);

    let sql = build_select_sql(&scan);
    let tupdesc = unsafe { build_tupdesc(&scan.cols) };

    let rows: Vec<PgHeapTuple<'static, AllocatedByRust>> = Spi::connect(|c| {
        let res = c.select(&sql, None, None).expect("SPI select failed");
        let mut out = Vec::with_capacity(res.len().unwrap_or(32));
        for row in res {
            unsafe {
                out.push(row_to_heap_tuple(&row, &scan.cols, &tupdesc));
            }
        }
        out
    });

    SetOfIterator::new(rows.into_iter())
}

/// -------------------------------
/// substrait_exec_wrapper (no AS)
/// -------------------------------
#[pg_extern]
fn substrait_exec_wrapper(
    plan_json: JsonB,
) -> SetOfIterator<'static, PgHeapTuple<'static, AllocatedByRust>> {
    let plan: Plan = serde_json::from_value::<JsonValue>(plan_json.0)
        .map_err(|e| error!("{}", e))
        .unwrap();
    let scan = extract_scan(&plan);

    // Create temp table for rowtype
    let temp_name = "__substrait_rt__";
    let create_cols = scan
        .cols
        .iter()
        .map(|c| {
            format!(
                "{} {}",
                pgrx::name::escape_ident(&c.name),
                type_oid_to_sql(c.oid)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let create_sql = format!(
        "CREATE TEMP TABLE {} ({}) ON COMMIT DROP",
        temp_name, create_cols
    );

    Spi::connect(|c| {
        let _ = c.update(&format!("DROP TABLE IF EXISTS {}", temp_name), None, None);
        c.update(&create_sql, None, None)
            .expect("CREATE TEMP TABLE failed");
    });

    let q = format!(
        "SELECT * FROM substrait_exec_raw($1)::pg_temp.{}",
        temp_name
    );
    let args = vec![(plan_json.to_string(), pg_sys::TEXTOID)];

    // Grab tupledesc of temp table
    let tupdesc = Spi::connect(|c| {
        let probe = format!("SELECT * FROM pg_temp.{} LIMIT 0", temp_name);
        let tbl = c.select(&probe, None, None).expect("probe failed");
        let tt = tbl.first().expect("no tuple table");
        unsafe { PgTupleDesc::from_pg_copy(tt.tupdesc()) }
    });

    let rows: Vec<PgHeapTuple<'static, AllocatedByRust>> = Spi::connect(|c| {
        let res = c
            .select_with_args(&q, Some(&args), None)
            .expect("wrapper select failed");
        let mut out = Vec::with_capacity(res.len().unwrap_or(32));
        for row in res {
            unsafe {
                out.push(row_to_heap_tuple(&row, &scan.cols, &tupdesc));
            }
        }
        out
    });

    SetOfIterator::new(rows.into_iter())
}

/// Map OID to SQL type string for CREATE TEMP TABLE
fn type_oid_to_sql(oid: pg_sys::Oid) -> &'static str {
    use pg_sys::*;
    match oid {
        BOOLOID => "bool",
        INT4OID => "int4",
        INT8OID => "int8",
        FLOAT4OID => "float4",
        FLOAT8OID => "float8",
        TEXTOID => "text",
        BYTEAOID => "bytea",
        DATEOID => "date",
        TIMESTAMPTZOID => "timestamptz",
        _ => "text",
    }
}

#[pg_extern]
fn _PG_init() {}
#[pg_extern]
fn _PG_fini() {}
