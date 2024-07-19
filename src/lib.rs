
pub mod server;


pub fn run_server() {
    println!("running server");
    let _ = server::server::run();
    
}


pub fn run_client() {
    println!("running client");
}