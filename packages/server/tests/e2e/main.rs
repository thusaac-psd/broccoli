// Tests are clients: they read the wall clock the way a contestant's machine
// does. The server itself keeps time by the platform clock.
#![allow(clippy::disallowed_methods)]

mod common;
mod core;
mod judging;
mod plugins;
