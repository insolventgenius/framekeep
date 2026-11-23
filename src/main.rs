mod export;
mod library;
mod lut;
mod native;
mod store;

fn main() {
    let folder = std::env::args_os().nth(1);
    native::run(folder.as_deref());
}
