//! Check the server the app syncs from: cargo run -p finance-core --example health
fn main() -> anyhow::Result<()> {
    let server = finance_core::cache::server_url();
    let rtt = finance_core::sync::healthz(&server)?;
    println!("{server} ok in {} ms", rtt.as_millis());
    Ok(())
}
