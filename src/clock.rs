use std::fmt;

use chrono::{DateTime, Utc};

pub trait Clock: Send + Sync + fmt::Debug {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
