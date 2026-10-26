use async_trait::async_trait;
use pingora::prelude::*;

struct Bench;

#[async_trait]
impl ProxyHttp for Bench {
    type CTX = ();
    fn new_ctx(&self) -> Self::CTX {}

    async fn upstream_peer(&self, _session: &mut Session, _ctx: &mut ()) -> Result<Box<HttpPeer>> {
        Ok(Box::new(HttpPeer::new("127.0.0.1:9100", false, String::new())))
    }

    async fn upstream_request_filter(
        &self,
        _session: &mut Session,
        req: &mut RequestHeader,
        _ctx: &mut Self::CTX,
    ) -> Result<()> {
        // Same as the other servers: /proxy/x reaches the upstream as /x.
        let path = req.uri.path().strip_prefix("/proxy").unwrap_or("/").to_string();
        let path = if path.is_empty() { "/".to_string() } else { path };
        req.set_uri(path.parse().unwrap());
        Ok(())
    }
}

fn main() {
    let mut server = Server::new(Some(Opt::parse_args())).unwrap();
    server.bootstrap();
    let mut proxy = http_proxy_service(&server.configuration, Bench);
    proxy.add_tcp("0.0.0.0:9006");
    server.add_service(proxy);
    server.run_forever();
}
