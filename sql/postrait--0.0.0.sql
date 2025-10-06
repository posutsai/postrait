-- Hand-written SQL registration for C-ABI function
CREATE FUNCTION my_dynamic_set()
RETURNS SETOF record
AS 'MODULE_PATHNAME', 'my_dynamic_set'
LANGUAGE C VOLATILE;

