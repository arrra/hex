pub fn double(x: i32) -> i32 { x * 2 }
pub fn generic_max<T: PartialOrd>(a: T, b: T) -> T { if a > b { a } else { b } }
macro_rules! call_double { ($x:expr) => { crate::ops::double($x) }; }
pub fn macro_caller() -> i32 { call_double!(21) }      // call site inside macro — the hard case
pub fn fmt_user(name: &str) -> String { format!("user:{}", double(name.len() as i32)) }
