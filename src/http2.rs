use crate::tcp::get_tcp_rtt;
use crate::ARGS;
use anyhow::Result;
use bytes::Bytes;
use h2::server::SendResponse;
use h2::{Ping, PingPong, RecvStream};
use http::header::{ACCESS_CONTROL_ALLOW_ORIGIN, CONTENT_TYPE};
use http::Request;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info};

#[tracing::instrument(fields(uri = %request.uri().path(), ip = %addr.ip()), skip_all)]
pub async fn handle(
    request: &mut Request<RecvStream>,
    respond: &mut SendResponse<Bytes>,
    raw_fd: i32,
    addr: SocketAddr,
    ping_pong: Arc<Mutex<PingPong>>,
) -> Result<()> {
    if request.uri().path() != "/" {
        error!(?request, "not found");
        let response = http::Response::builder()
            .status(http::StatusCode::NOT_FOUND)
            .header(ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .body(())?;
        let _ = respond.send_response(response, true)?;
        return Ok(());
    }

    let header_frame = http::Response::builder()
        .status(http::StatusCode::OK)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(())?;
    let mut send = respond.send_response(header_frame, false)?;

    let (tls_rtts, tls_rtt_dev) = tls_rtt(ping_pong).await?;
    let tls_rtt_min = detect_anomalies(&tls_rtts);
    let (tcp_rtt_ms, tcp_rtt_dev_ms) = get_tcp_rtt(raw_fd);

    // TODO: type convert
    let is_proxy = tls_rtt_min - tcp_rtt_ms as f32 > 2f32 * tcp_rtt_dev_ms;

    info!(?is_proxy, ?tcp_rtt_ms, ?tls_rtts, ?tls_rtt_min, "result");

    Ok(send.send_data(
        json!({
            "is_proxy": is_proxy,
            "ip": addr.ip().to_string(),
            "tcp_rtt_ms": tcp_rtt_ms,
            "tcp_rtt_dev_ms": tcp_rtt_dev_ms,
            "http2_rtt_ms": tls_rtts,
            "http2_rtt_min_ms": tls_rtt_min,
        })
            .to_string()
            .into(),
        true,
    )?)
}

#[tracing::instrument(skip_all)]
async fn tls_rtt(ping_pong: Arc<Mutex<PingPong>>) -> Result<(Vec<f32>, f32)> {
    let mut ping = ping_pong.lock().await;
    let mut tls_rtts = vec![];
    for index in 0..5 {
        let instant = tokio::time::Instant::now();
        ping.ping(Ping::opaque()).await?;
        let duration = instant.elapsed().as_micros() as f32 / 1000_f32;
        debug!(?duration, %index, "ping");
        tls_rtts.push(duration);
    }
    let tls_rtt = tls_rtts.iter().sum::<f32>() / tls_rtts.len() as f32;
    let tls_rtt_dev = calculate_variance(&tls_rtts, tls_rtt).sqrt() as f32;
    Ok((tls_rtts, tls_rtt_dev))
}

#[tracing::instrument]
fn detect_anomalies(data: &[f32]) -> f32 {
    let min = data.iter().min_by(
        |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
    ).unwrap_or(&f32::INFINITY);
    info!(?data, ?min);
    *min
}

fn calculate_variance(data: &[f32], mean: f32) -> f32 {
    data.iter().map(|&x| (x - mean).powi(2)).sum::<f32>() / data.len() as f32
}
