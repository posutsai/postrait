use pgrx::pg_sys;
use pgrx::pg_sys::FormData_pg_enum;
use pgrx::prelude::*;

pgrx::pg_module_magic!();

/// Dynamic SETOF RECORD example
///
/// SQL call:
/// ```sql
/// SELECT * FROM my_dynamic_set() AS t(msg text, num int);
/// ```
#[pg_extern(
    sql = "CREATE FUNCTION my_dynamic_set() RETURNS SETOF record AS 'MODULE_PATHNAME', 'my_dynamic_set' LANGUAGE c VOLATILE;"
)]
unsafe fn my_dynamic_set(fcinfo: pg_sys::FunctionCallInfo) {
    // Cast to ReturnSetInfo
    let rsinfo = fcinfo.as_ref().unwrap().resultinfo as *mut pg_sys::ReturnSetInfo;
    if rsinfo.is_null() {
        pgrx::error!("set-returning function called in wrong context");
    }

    let tupdesc = (*rsinfo).setDesc;
    let tuplestore = (*rsinfo).setResult;

    // Example row
    let msg = std::ffi::CString::new("hello from PG17 + pgrx 0.14").unwrap();
    let values = [
        pg_sys::CStringGetDatum(msg.as_ptr()), // text
        pg_sys::Int32GetDatum(2025),           // int
    ];
    let nulls = [false, false];

    // Insert into tuplestore
    pg_sys::tuplestore_putvalues(tuplestore, tupdesc, values.as_ptr(), nulls.as_ptr());
}
