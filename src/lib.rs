// hyper = { version = "1", features = ["full"] }
// tokio = { version = "1", features = ["full"] }
// http-body-util = "0.1"
// hyper-util = { version = "0.1", features = ["full"] }
//
/*

	port
	Option<certificates>
	f: request -> +
		HTTP response
		WebSocket handler
			f: Message -> Option<Message>
		WebSocket handler 2
			f: WS sink, WS stream -> ()

 */
//#![feature(trait_alias)]

use hyper::Request;
use hyper::Response;
//use std::convert::Infallible;
use tungstenite::protocol::Message;
use hyper::body::Incoming;
use http_body_util::Full;
//use http_body_util::Empty;
use hyper::body::Bytes;
use hyper::service::service_fn;
use hyper::server::conn::http1::Builder;
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use tokio::net::TcpListener;
//use tokio::net::TcpStream;
use futures_util::sink::SinkExt;
use futures_util::stream::StreamExt;
use tokio_tungstenite::WebSocketStream;
use futures::stream::SplitSink;
//use std::pin::Pin;
//use std::future::Future;

//macro_rules! why {
//	() => tokio::io::AsyncRead + tokio::io::AsyncWrite + futures_util::Sink<What>
//}

pub trait Why: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}

impl Why for tokio::net::TcpStream {}


pub struct WebSocketEventDriven {
	pub message_handler: Box<dyn (Fn(Message) -> Option<Message>) + Send + Sync>,
	pub quit_handler: Option<Box<dyn FnOnce() + Send + Sync>>,
	pub req: Request<Incoming>,
}
pub struct WebSocketQueues<S: Why> { // where S might be tokio::net::TcpStream
	pub callback: Box<dyn FnOnce(SplitSink<WebSocketStream<S>, Message>, tokio::sync::mpsc::Receiver<Message>) + Send + Sync>,
	pub req: Request<Incoming>,
}

pub enum Potato<S: Why> {
	HTTPResponse(Response<Full<Bytes>>),
	WebSocketHandler(WebSocketEventDriven),
	WebSocketQueues(WebSocketQueues<S>),
}

pub fn respond_file<S: Why>(s: &str) -> Potato<S> {
	let maybefile = std::fs::File::open(format!("html/{}", s));
	match maybefile {
		Ok(mut file) => {
			let mut buf = Vec::new();
			std::io::Read::read_to_end(&mut file, &mut buf).unwrap();
			if &s[s.len()-2..] == "js" {
				Potato::HTTPResponse(Response::builder()
				.status(200)
				.header("Content-Type", "application/javascript")
				.body(Full::new(Bytes::from(buf))).unwrap()
				)
			} else {
				Potato::HTTPResponse(Response::new(Full::new(Bytes::from(buf))))
			}
		},
		Err(_) => {
			Potato::HTTPResponse(Response::builder()
			.status(500)
			.body(Full::new(Bytes::from("500 Eroor.  file not found but it's our fault"))).unwrap())
		}
	}
}

pub async fn serve_blocking<F, T, Fut>(
	port: u16,
	data: T,
	f: F/*Box<dyn (Fn(&mut Request<Incoming>) -> Potato) + std::marker::Send + 'static + Clone + std::marker::Sync>*/
)
//where F: core::future::Future + Send + 'static
where F: (Fn(Request<Incoming>, T) -> Fut) + std::marker::Send + 'static + Clone + std::marker::Sync,
T: Clone + Send + Sync + 'static,
Fut: core::future::Future<Output = Potato<tokio::net::TcpStream>> + Send,
{
	println!("making listener");
	let addr = SocketAddr::from(([0, 0, 0, 0], port));
	let listener = TcpListener::bind(addr).await.unwrap();
	println!("made listener");
	loop {
		println!("listener looking for stream");
		let (stream, _) = listener.accept().await.unwrap();
		println!("listener got stream");
		let f = (&f).clone();
		let t = data.clone();
		tokio::task::spawn(async move {
			serve_blocking_generic_inner(stream, t, f).await;
		});
	}
}

async fn serve_blocking_generic_inner<S, T, F, Fut>(
	stream: S,
	t: T,
	f: F
)
where F: (Fn(Request<Incoming>, T) -> Fut) + std::marker::Send + 'static + Clone + std::marker::Sync,
T: Clone + Send + Sync + 'static,
Fut: core::future::Future<Output = Potato<S>> + Send,
S: Why + 'static
{
	let io = hyper_util::rt::TokioIo::new(stream);
	if let Err(err) = Builder::new()
	.serve_connection(io, service_fn(
		|req: Request<hyper::body::Incoming>| async {
			let f = (&f).clone();
			let t = t.clone();
			let retkey = {
				let headers = req.headers();
				let reqkey = headers.get("Sec-WebSocket-Key").clone();
				if let Some(reqkey) = reqkey {
					tungstenite::handshake::derive_accept_key(reqkey.as_bytes())
				} else {
					"".to_string()
				}
			};
			match f(req, t).await {
				Potato::HTTPResponse(r) => { return Ok::<_, hyper::http::Error>(r); },
				Potato::WebSocketHandler(wsed) => { return handle_ws_eventdriven::<S>(wsed, retkey); },
				Potato::WebSocketQueues(wsqf) => { return handle_ws_polldriven(wsqf, retkey); },
			}
		}
	))
	.with_upgrades()
	.await
	{
		println!("Error serving connection: {:?}", err);
	}
}

fn handle_ws<F, Fut, S>(
	mut req: Request<Incoming>,
	retkey: String,
	f: Box<F>
)
-> Result<Response<Full<Bytes>>, hyper::http::Error>
where F: (FnOnce(tokio_tungstenite::WebSocketStream<S>) -> Fut) + Send + 'static,
Fut: core::future::Future<Output = ()> + Send,
S: Why + 'static
{
	tokio::task::spawn(async move {
		match hyper::upgrade::on(&mut req).await {
			Ok(upgraded) => {
				let parts: hyper::upgrade::Parts<TokioIo<S>> = upgraded.downcast().unwrap();
				let stream = parts.io.into_inner();
				//let mut wsock = tokio_tungstenite::accept_async(stream).await.unwrap();
				let wsock = tokio_tungstenite::WebSocketStream::from_raw_socket(
					stream,
					tungstenite::protocol::Role::Server,
					None).await;
				f(wsock).await;
			}
			Err(e) => eprintln!("upgrade error: {}", e),
		}
	});
	return Ok(hyper::Response::builder()
	.status(101)
	.header("Connection", "Upgrade")
	.header("Upgrade", "websocket")
	.header("Sec-WebSocket-Accept", retkey)
	.body(Full::new(Bytes::from("")))?);
}

fn handle_ws_polldriven<S>(wsqf: WebSocketQueues<S>, retkey: String)
-> Result<Response<Full<Bytes>>, hyper::http::Error>
where S: Why + 'static
{
	let WebSocketQueues { callback, req } = wsqf;
	handle_ws(req, retkey, Box::new(|wsock: WebSocketStream<S>| async move {
		let (sink, mut stream) = wsock.split();
		let (sender, receiver) = tokio::sync::mpsc::channel(100);
		tokio::spawn(async move {
			loop {
				let msg = stream.next().await;
				match msg {
					Some(Ok(msg)) => { sender.send(msg).await.unwrap(); },
					_ => { break; }
				}
				sender.send(tungstenite::Message::Close(None)).await.unwrap();
			}
		});
		callback(sink, receiver);
	}))
}

fn handle_ws_eventdriven<S: Why + 'static>(wsed: WebSocketEventDriven, retkey: String)
-> Result<Response<Full<Bytes>>, hyper::http::Error>
where S: Why
{
	let WebSocketEventDriven { message_handler: fws, quit_handler: fquit, req } = wsed;
	handle_ws(req, retkey, Box::new(|mut wsock: WebSocketStream<S>| async move {
		let fws = fws;
		let fquit = fquit;
		loop {
			let Some(Ok(val)) = wsock.next().await else {
				// client disconnected
				break;
			};
			// val: Message
			let mmsg = { fws(val).clone() };
			if let Some(msg) = mmsg {
				wsock.send(msg).await.unwrap();
			}
		}
		if let Some(fquit) = fquit {
			fquit();
		}
	}))
}


// private stuff now ////////////////////////////////////

/*
 * ok so what is the difference between
 * async move{}
 * || async move {}
 * async move || {}
 */

/*

good:
	Box::new(|| async move {})
		Box<F>
		where F: (FnOnce(tokio_tungstenite::WebSocketStream<S>) -> Fut) + Send + 'static,
		Fut: core::future::Future<Output = ()>

bad:
	async || {}



 */
