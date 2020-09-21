// TODO switch to something else
pub type BoxResult<T> = Result<T, Box<dyn std::error::Error>>;
