class SmtpSink < Formula
  desc "Minimal SMTP sink for local development and testing"
  homepage "https://github.com/jimmystridh/smtp-sink"
  url "https://github.com/jimmystridh/smtp-sink/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "REPLACE_WITH_ACTUAL_SHA256"
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  def post_install
    (var/"log/smtp-sink").mkpath
  end

  service do
    run [opt_bin/"smtp-sink"]
    keep_alive true
    log_path var/"log/smtp-sink/smtp-sink.log"
    error_log_path var/"log/smtp-sink/smtp-sink.log"
  end

  test do
    port = free_port
    fork do
      exec bin/"smtp-sink", "--smtp-port", port.to_s, "--http-port", (port + 1).to_s
    end
    sleep 2
    assert_match "localhost", shell_output("nc -z 127.0.0.1 #{port} && echo localhost")
  end
end
