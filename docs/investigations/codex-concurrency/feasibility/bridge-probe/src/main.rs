#[path = "../../native_bridge.rs"]
mod native_bridge;
use serde_json::{Value, json};
use std::io::{self, Write};

fn main() -> io::Result<()> {
    native_bridge::enable_from_env()?;
    let args: Vec<String> = std::env::args().collect();
    let method = &args[1];
    let thread = args
        .get(2)
        .filter(|v| v.as_str() != "-")
        .map(String::as_str);
    if method == "selection" {
        native_bridge::selection(None, thread.unwrap())?;
        println!("{}", json!({"phase":"selected"}));
        return Ok(());
    }
    if !native_bridge::requires_gate(method)? {
        println!("{}", json!({"phase":"ungated-provider-read"}));
        return Ok(());
    }
    let mut ticket = native_bridge::Ticket::begin(method, thread)?;
    println!("{}", json!({"phase":"prepared"}));
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let response: Value = serde_json::from_str(&line).map_err(io::Error::other)?;
    ticket.complete(
        response["outcome"].as_str().unwrap_or("unknown"),
        response["thread"].as_str(),
    )?;
    println!("{}", json!({"phase":"committed"}));
    Ok(())
}
