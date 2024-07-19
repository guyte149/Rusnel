
pub mod server;
pub mod client;


pub fn run_server() {
    println!("running server");
    match server::server::run() {
        Ok(_) => {},
        Err(e) => {println!("an error occurred: {}", e)}
    }
}


pub fn run_client() {
    println!("running client");
    match client::client::run() {
        Ok(_) => {},
        Err(e) => {println!("an error occurred: {}", e)}
    }
}