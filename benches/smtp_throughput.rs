//! SMTP throughput benchmarks.
//!
//! Run with: `cargo bench`

#![allow(clippy::significant_drop_tightening)]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use lettre::transport::smtp::client::Tls;
use lettre::{Message, SmtpTransport, Transport};
use std::time::Duration;
use tokio::runtime::Runtime;

/// Start the SMTP sink server and return the address.
async fn start_server() -> (smtp_sink::RunningServers, u16) {
    let opts = smtp_sink::SinkOptions {
        smtp_port: Some(0), // Use random available port
        http_port: Some(0),
        max: Some(10000),
        ..Default::default()
    };
    let servers = smtp_sink::start_sink(opts).await.unwrap();
    let port = servers.smtp_addr.port();
    (servers, port)
}

/// Send a single email synchronously.
fn send_email(port: u16, subject: &str) {
    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject(subject)
        .body("This is a test email body for benchmarking.".to_string())
        .unwrap();

    let mailer = SmtpTransport::builder_dangerous("127.0.0.1")
        .port(port)
        .tls(Tls::None)
        .build();

    mailer.send(&email).unwrap();
}

/// Send a larger email with HTML content.
fn send_large_email(port: u16, subject: &str) {
    let html_body = r"
        <!DOCTYPE html>
        <html>
        <head><title>Test Email</title></head>
        <body>
            <h1>Hello from the benchmark!</h1>
            <p>This is a test email with HTML content for benchmarking purposes.</p>
            <ul>
                <li>Item 1</li>
                <li>Item 2</li>
                <li>Item 3</li>
            </ul>
            <p>Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor 
            incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud 
            exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.</p>
        </body>
        </html>
    ";

    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject(subject)
        .header(lettre::message::header::ContentType::TEXT_HTML)
        .body(html_body.to_string())
        .unwrap();

    let mailer = SmtpTransport::builder_dangerous("127.0.0.1")
        .port(port)
        .tls(Tls::None)
        .build();

    mailer.send(&email).unwrap();
}

fn benchmark_single_email(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (servers, port) = rt.block_on(start_server());

    let mut group = c.benchmark_group("smtp_single_email");
    group.throughput(Throughput::Elements(1));
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("send_simple_email", |b| {
        let mut i = 0;
        b.iter(|| {
            i += 1;
            send_email(port, &format!("Benchmark email {i}"));
        });
    });

    group.bench_function("send_html_email", |b| {
        let mut i = 0;
        b.iter(|| {
            i += 1;
            send_large_email(port, &format!("Benchmark HTML email {i}"));
        });
    });

    group.finish();
    rt.block_on(servers.stop());
}

fn benchmark_batch_emails(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (servers, port) = rt.block_on(start_server());

    let mut group = c.benchmark_group("smtp_batch_emails");
    group.measurement_time(Duration::from_secs(15));

    for batch_size in [10_u64, 50, 100] {
        group.throughput(Throughput::Elements(batch_size));
        group.bench_with_input(
            BenchmarkId::new("batch", batch_size),
            &batch_size,
            |b, &size| {
                let mut batch = 0;
                b.iter(|| {
                    batch += 1;
                    for i in 0..size {
                        send_email(port, &format!("Batch {batch} email {i}"));
                    }
                });
            },
        );
    }

    group.finish();
    rt.block_on(servers.stop());
}

fn benchmark_concurrent_connections(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (servers, port) = rt.block_on(start_server());

    let mut group = c.benchmark_group("smtp_concurrent");
    group.measurement_time(Duration::from_secs(15));

    for num_connections in [2_u64, 4, 8] {
        group.throughput(Throughput::Elements(num_connections));
        group.bench_with_input(
            BenchmarkId::new("connections", num_connections),
            &num_connections,
            |b, &n| {
                let mut batch = 0;
                b.iter(|| {
                    batch += 1;
                    let handles: Vec<_> = (0..n)
                        .map(|i| {
                            let subject = format!("Concurrent batch {batch} conn {i}");
                            std::thread::spawn(move || {
                                send_email(port, &subject);
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
    rt.block_on(servers.stop());
}

criterion_group!(
    benches,
    benchmark_single_email,
    benchmark_batch_emails,
    benchmark_concurrent_connections,
);
criterion_main!(benches);
