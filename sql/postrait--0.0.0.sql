-- Hand-written SQL registration for C-ABI function
CREATE FUNCTION my_dynamic_set(
	"json_str" TEXT /* &str */
)
RETURNS SETOF record
AS 'MODULE_PATHNAME', 'my_dynamic_set'
LANGUAGE C VOLATILE;

CREATE  FUNCTION "convert_substrait_json_to_runnable_sql"(
	"json_str" TEXT /* &str */
) RETURNS TEXT /* alloc::string::String */
STRICT
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'convert_substrait_json_to_runnable_sql_wrapper';

