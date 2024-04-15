use futures::StreamExt;
use net_route::Handle;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let handle = Handle::new()?;
    let stream = handle.route_listen_stream();

    futures::pin_mut!(stream);

    println!("Listening for route events, press Ctrl+C to cancel...");
    while let Some(event) = stream.next().await {
        println!("event:{:?}", event);
        if let Some(route) = handle.default_route().await? {
            println!("Default route:\n{:?}", route);
        } else {
            println!("No default route found!");
        }
    }
    Ok(())
}
