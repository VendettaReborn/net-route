use net_route::Handle;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let handle = Handle::new()?;
    let routes = handle.list().await?;

    for route in routes {
        println!("{:?}", route);
    }
    Ok(())
}
