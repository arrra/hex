pub trait Area { fn area(&self) -> f64; }
pub struct Circle { pub r: f64 }
impl Area for Circle { fn area(&self) -> f64 { std::f64::consts::PI * self.r * self.r } }
pub struct Sq { pub s: f64 }
impl Area for Sq { fn area(&self) -> f64 { self.s * self.s } }
pub fn total_area(items: &[&dyn Area]) -> f64 { items.iter().map(|i| i.area()).sum() }
