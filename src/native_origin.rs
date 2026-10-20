//! Private, asset-only loopback origin for the native WebKit client.
//! RPC continues over its in-process IPC channel; this listener exposes no RPC.
use super::*;

pub struct NativeOrigin {
    url: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl NativeOrigin {
    pub fn url(&self) -> &str {
        &self.url
    }
}
impl Drop for NativeOrigin {
    fn drop(&mut self) {
        if let Some(stop) = self.shutdown.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn serve(html: String) -> Result<NativeOrigin> {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .context("bind native asset origin")?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let path = format!("/{:032x}/", rand::random::<u128>());
    let url = format!("http://127.0.0.1:{port}{path}");
    let (shutdown, mut stop) = tokio::sync::oneshot::channel();
    let worker=std::thread::Builder::new().name("native-origin".into()).spawn(move||{
        let Ok(runtime)=tokio::runtime::Builder::new_current_thread().enable_all().build()else{return;};
        runtime.block_on(async move {
            let Ok(listener)=TcpListener::from_std(listener)else{return;};
            let html:Arc<str>=Arc::from(html);let permits=Arc::new(Semaphore::new(16));let mut connections=tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _=&mut stop=>break,
                    Some(_)=connections.join_next(),if !connections.is_empty()=>{},
                    accepted=listener.accept()=>{
                        let Ok((stream,_))=accepted else {break;};
                        let Ok(permit)=permits.clone().try_acquire_owned()else{continue;};
                        let (html,path)=(html.clone(),path.clone());
                        connections.spawn(async move {let _permit=permit;let _=handle_asset(stream,port,&path,&html).await;});
                    }
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some(){}
        });
    })?;
    Ok(NativeOrigin {
        url,
        shutdown: Some(shutdown),
        worker: Some(worker),
    })
}

async fn handle_asset(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    port: u16,
    path: &str,
    html: &str,
) -> Result<()> {
    let request = match timeout(REQUEST_TIMEOUT, read_request_limited(&mut stream, 0)).await {
        Ok(Ok(request)) => request,
        Ok(Err(HttpError(status, message))) => {
            return respond(&mut stream, status, "text/plain", message.as_bytes()).await
        }
        Err(_) => return respond(&mut stream, 408, "text/plain", b"Request timed out").await,
    };
    if let Err(HttpError(status, message)) = validate_origin(&request, port) {
        return respond(&mut stream, status, "text/plain", message.as_bytes()).await;
    }
    if request.method != "GET" {
        return respond(&mut stream, 405, "text/plain", b"Method not allowed").await;
    }
    if request.path != path {
        return respond(&mut stream, 404, "text/plain", b"Not found").await;
    }
    respond(
        &mut stream,
        200,
        "text/html; charset=utf-8",
        html.as_bytes(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn request(port: u16, head: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        stream.write_all(head.as_bytes()).await.unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.unwrap();
        String::from_utf8(bytes).unwrap()
    }
    #[tokio::test]
    async fn native_origin_serves_only_its_unpredictable_path_and_stops_on_drop() {
        let origin = serve("<!doctype html><title>Isolated native UI</title>".into()).unwrap();
        let url = reqwest::Url::parse(origin.url()).unwrap();
        let port = url.port().unwrap();
        let host = format!("127.0.0.1:{port}");
        let ok = request(
            port,
            &format!("GET {} HTTP/1.1\r\nHost: {host}\r\n\r\n", url.path()),
        )
        .await;
        assert!(ok.starts_with("HTTP/1.1 200") && ok.contains("Isolated native UI"));
        for path in ["/", "/rpc", "/events"] {
            assert!(request(
                port,
                &format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n\r\n")
            )
            .await
            .starts_with("HTTP/1.1 404"));
        }
        assert!(request(port,&format!("POST /rpc HTTP/1.1\r\nHost: {host}\r\nOrigin: http://{host}\r\nContent-Length: 1\r\n\r\nx")).await.starts_with("HTTP/1.1 413"));
        assert!(request(
            port,
            &format!(
                "GET {} HTTP/1.1\r\nHost: hostile.example\r\n\r\n",
                url.path()
            )
        )
        .await
        .starts_with("HTTP/1.1 403"));
        assert!(request(
            port,
            &format!(
                "GET {} HTTP/1.1\r\nHost: {host}\r\nOrigin: https://hostile.example\r\n\r\n",
                url.path()
            )
        )
        .await
        .starts_with("HTTP/1.1 403"));
        drop(origin);
        assert!(
            tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .is_err()
        );
    }
}
