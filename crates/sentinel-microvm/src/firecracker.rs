//! Low-Level Firecracker-Integration: KVM-Check, Prozess-Lebenszyklus und ein minimaler
//! HTTP/1.1-ueber-Unix-Domain-Socket-Client fuer die Firecracker-API (PUT/PATCH/GET mit JSON).
//!
//! Bewusst dependency-frei (nur std + serde_json): die Firecracker-API ist simpel genug
//! (kleine JSON-Bodies, 2xx/4xx-Antworten), dass ein robuster Content-Length-Reader genuegt.

use std::io::{BufRead, BufReader, Read, Write};
#[cfg(test)]
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

/// Pfad zum KVM-Device.
pub const KVM_DEVICE: &str = "/dev/kvm";

/// Prueft KVM-Verfuegbarkeit. Liefert einen sauberen Fehler wenn `/dev/kvm` fehlt oder nicht
/// beschreibbar ist (#417 AC-4: "sauberer Fehler wenn KVM fehlt").
pub fn ensure_kvm_available() -> Result<()> {
    let path = Path::new(KVM_DEVICE);
    if !path.exists() {
        bail!(
            "KVM nicht verfuegbar: {KVM_DEVICE} fehlt — die microVM-Runtime benoetigt \
             Hardware-Virtualisierung (nested-virt aktivieren / auf einer KVM-faehigen Maschine ausfuehren)"
        );
    }
    // Firecracker oeffnet /dev/kvm mit O_RDWR — Schreibrechte vorab pruefen.
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map(|_| ())
        .map_err(|e| {
            anyhow!("KVM nicht nutzbar: {KVM_DEVICE} nicht oeffenbar ({e}) — Rechte/Gruppe 'kvm' pruefen")
        })
}

/// True wenn KVM nutzbar ist (ohne Fehler zu werfen).
pub fn kvm_available() -> bool {
    ensure_kvm_available().is_ok()
}

/// Antwort eines Firecracker-API-Aufrufs.
#[derive(Debug)]
pub struct ApiResponse {
    pub status: u16,
    pub body: String,
}

fn connect_with_retry(sock: &Path, timeout: Duration) -> Result<UnixStream> {
    let deadline = Instant::now() + timeout;
    loop {
        match UnixStream::connect(sock) {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(anyhow!(
                        "Firecracker API-Socket {} nicht erreichbar: {e}",
                        sock.display()
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Fuehrt einen HTTP-Request gegen den Firecracker-API-Socket aus und liest die Antwort
/// vollstaendig (robustes Content-Length-Parsing, unabhaengig von keep-alive).
pub fn api_request(sock: &Path, method: &str, path: &str, body: &str) -> Result<ApiResponse> {
    let mut stream = connect_with_retry(sock, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .context("Firecracker API: Request schreiben")?;
    stream.flush().ok();

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .context("Firecracker API: Statuszeile lesen")?;
    let status = parse_status_code(&status_line)
        .ok_or_else(|| anyhow!("ungueltige Firecracker-Statuszeile: {status_line:?}"))?;

    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
    }

    let mut body_buf = vec![0u8; content_length];
    if content_length > 0 {
        reader
            .read_exact(&mut body_buf)
            .context("Firecracker API: Body lesen")?;
    }

    Ok(ApiResponse {
        status,
        body: String::from_utf8_lossy(&body_buf).into_owned(),
    })
}

fn parse_status_code(status_line: &str) -> Option<u16> {
    // Format: "HTTP/1.1 204 No Content"
    status_line.split_whitespace().nth(1)?.parse().ok()
}

fn put(sock: &Path, path: &str, body: &str) -> Result<()> {
    let resp = api_request(sock, "PUT", path, body)?;
    if (200..300).contains(&resp.status) {
        Ok(())
    } else {
        bail!("Firecracker PUT {path} -> {} {}", resp.status, resp.body)
    }
}

fn patch(sock: &Path, path: &str, body: &str) -> Result<()> {
    let resp = api_request(sock, "PATCH", path, body)?;
    if (200..300).contains(&resp.status) {
        Ok(())
    } else {
        bail!("Firecracker PATCH {path} -> {} {}", resp.status, resp.body)
    }
}

/// Ein laufender Firecracker-Prozess samt API-Socket. Wird beim Drop sauber beendet.
pub struct FirecrackerProcess {
    child: Child,
    api_sock: PathBuf,
    #[cfg(test)]
    fixture_stop: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    fixture_server: Option<JoinHandle<()>>,
}

impl FirecrackerProcess {
    /// Startet `firecracker --api-sock <sock>` und wartet, bis der API-Socket erreichbar ist.
    pub fn launch(firecracker_bin: &str, api_sock: &Path) -> Result<Self> {
        // Alten Socket entfernen, sonst scheitert Firecracker am bind.
        let _ = std::fs::remove_file(api_sock);
        let child = Command::new(firecracker_bin)
            .arg("--api-sock")
            .arg(api_sock)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("Firecracker-Binary '{firecracker_bin}' nicht startbar"))?;

        // Auf den API-Socket warten (Firecracker erstellt ihn asynchron).
        if let Err(e) = connect_with_retry(api_sock, Duration::from_secs(5)) {
            let mut proc = Self {
                child,
                api_sock: api_sock.to_path_buf(),
                #[cfg(test)]
                fixture_stop: None,
                #[cfg(test)]
                fixture_server: None,
            };
            let _ = proc.terminate();
            return Err(e).context("Firecracker API-Socket erschien nicht nach dem Start");
        }

        Ok(Self {
            child,
            api_sock: api_sock.to_path_buf(),
            #[cfg(test)]
            fixture_stop: None,
            #[cfg(test)]
            fixture_server: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn launch_fixture(api_sock: &Path) -> Result<Self> {
        if let Some(parent) = api_sock.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(api_sock).context("bind fixture API socket")?;
        listener
            .set_nonblocking(true)
            .context("set fixture API socket nonblocking")?;
        let fixture_stop = Arc::new(AtomicBool::new(false));
        let server_stop = Arc::clone(&fixture_stop);
        let fixture_server = std::thread::spawn(move || {
            while !server_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if server_stop.load(Ordering::Acquire) {
                            break;
                        }
                        let mut request = [0u8; 4096];
                        let _ = stream.read(&mut request);
                        let body = r#"{"state":"Running"}"#;
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        let child = match Command::new("/usr/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                fixture_stop.store(true, Ordering::Release);
                let _ = UnixStream::connect(api_sock);
                let _ = fixture_server.join();
                return Err(error).context("start Firecracker lifecycle fixture");
            }
        };
        Ok(Self {
            child,
            api_sock: api_sock.to_path_buf(),
            fixture_stop: Some(fixture_stop),
            fixture_server: Some(fixture_server),
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// True wenn der Firecracker-Prozess noch laeuft.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Beendet den Prozess, reap't ihn und raeumt den API-Socket auf.
    ///
    /// Fehler werden dem expliziten Stop-Pfad gemeldet, damit der Runtime-
    /// Besitzer die Workload fuer einen Retry behalten kann. `Drop` bleibt
    /// lediglich das best-effort Sicherheitsnetz.
    pub fn terminate(&mut self) -> Result<()> {
        match self
            .child
            .try_wait()
            .context("query Firecracker process state")?
        {
            Some(_) => {}
            None => {
                self.child.kill().context("kill Firecracker process")?;
                self.child.wait().context("reap Firecracker process")?;
            }
        }
        #[cfg(test)]
        if let Some(stop) = self.fixture_stop.take() {
            stop.store(true, Ordering::Release);
            let _ = UnixStream::connect(&self.api_sock);
        }
        #[cfg(test)]
        if let Some(server) = self.fixture_server.take() {
            server
                .join()
                .map_err(|_| anyhow!("Firecracker fixture server panicked"))?;
        }
        match std::fs::remove_file(&self.api_sock) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| {
                format!("remove Firecracker API socket {}", self.api_sock.display())
            }),
        }
    }
}

impl Drop for FirecrackerProcess {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

// --- High-Level Firecracker-API-Operationen ---

/// Setzt vCPU-Anzahl und Speichergroesse (MiB).
pub fn configure_machine(sock: &Path, vcpu_count: u32, mem_size_mib: u32) -> Result<()> {
    let body = serde_json::json!({
        "vcpu_count": vcpu_count,
        "mem_size_mib": mem_size_mib,
    })
    .to_string();
    put(sock, "/machine-config", &body)
}

/// Setzt den Boot-Source (Gast-Kernel-Image + boot_args).
pub fn configure_boot_source(sock: &Path, kernel_image_path: &str, boot_args: &str) -> Result<()> {
    let body = serde_json::json!({
        "kernel_image_path": kernel_image_path,
        "boot_args": boot_args,
    })
    .to_string();
    put(sock, "/boot-source", &body)
}

/// Konfiguriert das Root-Drive (rootfs).
pub fn configure_rootfs(
    sock: &Path,
    drive_id: &str,
    path_on_host: &str,
    is_read_only: bool,
) -> Result<()> {
    let body = serde_json::json!({
        "drive_id": drive_id,
        "path_on_host": path_on_host,
        "is_root_device": true,
        "is_read_only": is_read_only,
    })
    .to_string();
    put(sock, &format!("/drives/{drive_id}"), &body)
}

/// Konfiguriert ein vsock-Geraet (Host-UDS `uds_path`, Gast-CID `guest_cid`).
pub fn configure_vsock(sock: &Path, guest_cid: u32, uds_path: &str) -> Result<()> {
    let body = serde_json::json!({
        "guest_cid": guest_cid,
        "uds_path": uds_path,
    })
    .to_string();
    put(sock, "/vsock", &body)
}

/// Startet die microVM (Boot).
pub fn instance_start(sock: &Path) -> Result<()> {
    let body = serde_json::json!({ "action_type": "InstanceStart" }).to_string();
    put(sock, "/actions", &body)
}

/// Pausiert die laufende microVM (Voraussetzung fuer Snapshot).
pub fn pause(sock: &Path) -> Result<()> {
    patch(
        sock,
        "/vm",
        &serde_json::json!({ "state": "Paused" }).to_string(),
    )
}

/// Setzt eine pausierte microVM fort.
pub fn resume(sock: &Path) -> Result<()> {
    patch(
        sock,
        "/vm",
        &serde_json::json!({ "state": "Resumed" }).to_string(),
    )
}

/// Erzeugt einen Full-Snapshot (state-Datei + mem-Datei) ueber die Firecracker-Snapshot-API.
/// Die VM muss vorher pausiert sein.
pub fn create_snapshot(sock: &Path, snapshot_path: &str, mem_file_path: &str) -> Result<()> {
    let body = serde_json::json!({
        "snapshot_type": "Full",
        "snapshot_path": snapshot_path,
        "mem_file_path": mem_file_path,
    })
    .to_string();
    put(sock, "/snapshot/create", &body)
}

/// Laedt eine microVM aus einem Snapshot (state + mem). Bei `resume_vm` startet sie direkt.
pub fn load_snapshot(
    sock: &Path,
    snapshot_path: &str,
    mem_file_path: &str,
    resume_vm: bool,
) -> Result<()> {
    let body = serde_json::json!({
        "snapshot_path": snapshot_path,
        "mem_backend": { "backend_type": "File", "backend_path": mem_file_path },
        "enable_diff_snapshots": false,
        "resume_vm": resume_vm,
    })
    .to_string();
    put(sock, "/snapshot/load", &body)
}

/// Liefert den Instanz-Zustand laut Firecracker InstanceInfo (z.B. "Running", "Paused",
/// "Not started").
pub fn instance_state(sock: &Path) -> Result<String> {
    let resp = api_request(sock, "GET", "/", "")?;
    if !(200..300).contains(&resp.status) {
        bail!("Firecracker GET / -> {} {}", resp.status, resp.body);
    }
    let info: serde_json::Value =
        serde_json::from_str(&resp.body).unwrap_or(serde_json::Value::Null);
    Ok(info
        .get("state")
        .and_then(|s| s.as_str())
        .unwrap_or("unknown")
        .to_string())
}
