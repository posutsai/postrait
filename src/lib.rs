use pgrx::pg_sys;
use pgrx::prelude::*;
use std::ffi::CString;

pgrx::pg_module_magic!();

#[no_mangle]
pub static my_dynamic_set_finfo_record: pg_sys::Pg_finfo_record =
    pg_sys::Pg_finfo_record { api_version: 1 };

#[no_mangle]
pub extern "C" fn pg_finfo_my_dynamic_set() -> *const pg_sys::Pg_finfo_record {
    &my_dynamic_set_finfo_record
}

#[no_mangle]
pub extern "C-unwind" fn my_dynamic_set(fcinfo: pg_sys::FunctionCallInfo) -> pg_sys::Datum {
    unsafe {
        // 1. Get ReturnSetInfo
        let rsinfo = fcinfo.as_ref().unwrap().resultinfo as *mut pg_sys::ReturnSetInfo;
        if rsinfo.is_null() {
            pgrx::error!("called in wrong context");
        }

        // Must support materialize
        if ((*rsinfo).allowedModes & (pg_sys::SetFunctionReturnMode::SFRM_Materialize as i32)) == 0
        {
            pgrx::error!("function not allowed in this context");
        }

        // We will return a tuplestore
        (*rsinfo).returnMode = pg_sys::SetFunctionReturnMode::SFRM_Materialize;

        // Switch to executor's per-query memory context for allocations
        let ectx = (*rsinfo).econtext;
        let per_query_mcxt = (*ectx).ecxt_per_query_memory;
        let old_ctx = pg_sys::MemoryContextSwitchTo(per_query_mcxt);

        // Create tuplestore (respect random access)
        let random_access = ((*rsinfo).allowedModes
            & (pg_sys::SetFunctionReturnMode::SFRM_Materialize_Random as i32))
            != 0;
        let ts = pg_sys::tuplestore_begin_heap(random_access, false, pg_sys::work_mem);
        (*rsinfo).setResult = ts;

        // Get the result rowtype that the executor expects (handles both: AS (...) and default)
        let mut rettypeid: pg_sys::Oid = 0.into();
        let mut tupdesc_ptr: *mut pg_sys::TupleDescData = std::ptr::null_mut();
        let tfc = pg_sys::get_call_result_type(fcinfo, &mut rettypeid, &mut tupdesc_ptr);

        // We expect a record/composite rowtype here
        if tfc != pg_sys::TypeFuncClass::TYPEFUNC_COMPOSITE
            && tfc != pg_sys::TypeFuncClass::TYPEFUNC_RECORD
        {
            pg_sys::MemoryContextSwitchTo(old_ctx);
            pgrx::error!("unexpected result type for SRF");
        }

        // Bless if needed (safe even if already blessed)
        if !tupdesc_ptr.is_null() && (*tupdesc_ptr).tdrefcount == 0 {
            tupdesc_ptr = pg_sys::BlessTupleDesc(tupdesc_ptr);
        }
        if tupdesc_ptr.is_null() {
            pg_sys::MemoryContextSwitchTo(old_ctx);
            pgrx::error!("failed to obtain tuple descriptor");
        }

        (*rsinfo).setDesc = tupdesc_ptr;

        // Build values
        let msg = std::ffi::CString::new("hello from PG17 + pgrx 0.14").unwrap();
        let text_datum = pg_sys::PointerGetDatum(
            pg_sys::cstring_to_text(msg.as_ptr()) as *const std::ffi::c_void
        );
        let values = [text_datum, pg_sys::Int32GetDatum(2025)];
        let nulls = [false, false];

        // Emit one row using the SAME descriptor we put in setDesc
        pg_sys::tuplestore_putvalues(ts, tupdesc_ptr, values.as_ptr(), nulls.as_ptr());

        // Restore previous memory context
        pg_sys::MemoryContextSwitchTo(old_ctx);
        pg_sys::Datum::from(0)
    }
}
