//! TLS interop with OpenSSL.

#[allow(clippy::wildcard_imports)]
use crate::cli::*;

/// OpenSSL 3 for the TLS interop test (`NABI_OPENSSL`, `openssl` on
/// `PATH`, Homebrew's) and its minor version.
pub(crate) fn openssl3() -> Option<(PathBuf, u32)> {
    let mut cands: Vec<PathBuf> = std::env::var_os("NABI_OPENSSL")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    cands.push("openssl".into());
    cands.push("/opt/homebrew/opt/openssl@3/bin/openssl".into());
    cands.push("/usr/local/opt/openssl@3/bin/openssl".into());
    cands.into_iter().find_map(|p| {
        let out = Command::new(&p).arg("version").output().ok()?;
        let v = text(&out.stdout);
        let minor = v
            .strip_prefix("OpenSSL 3.")?
            .split('.')
            .next()?
            .parse()
            .ok()?;
        Some((p, minor))
    })
}

#[track_caller]
pub(crate) fn ossl(o: &Path, dir: &Path, args: &[&str]) {
    let out = Command::new(o)
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "openssl {args:?}: {}",
        text(&out.stderr)
    );
}

/// A CA (P-256) and leaves under it: `name.pem` / `name.key`.
pub(crate) fn interop_certs(o: &Path, dir: &Path) {
    let ec = ["-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256"];
    ossl(
        o,
        dir,
        &[&["genpkey"][..], &ec, &["-out", "ca.key"]].concat(),
    );
    ossl(
        o,
        dir,
        &[
            "req",
            "-x509",
            "-new",
            "-key",
            "ca.key",
            "-subj",
            "/CN=interop CA",
            "-days",
            "30",
            "-out",
            "ca.pem",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign",
        ],
    );
    let leaves: [(&str, &[&str], &str, &[&str]); 4] = [
        ("p256", &ec, "serverAuth", &["-days", "30"]),
        (
            "ed25519",
            &["-algorithm", "ED25519"],
            "serverAuth",
            &["-days", "30"],
        ),
        ("client", &ec, "clientAuth", &["-days", "30"]),
        (
            "expired",
            &ec,
            "serverAuth",
            &[
                "-not_before",
                "20200101000000Z",
                "-not_after",
                "20210101000000Z",
            ],
        ),
    ];
    for (name, alg, eku, validity) in leaves {
        let (key, csr, cert) = (
            format!("{name}.key"),
            format!("{name}.csr"),
            format!("{name}.pem"),
        );
        ossl(o, dir, &[&["genpkey"][..], alg, &["-out", &key]].concat());
        ossl(
            o,
            dir,
            &[
                "req",
                "-new",
                "-key",
                &key,
                "-subj",
                &format!("/CN={name}"),
                "-out",
                &csr,
            ],
        );
        let ext = format!(
            "basicConstraints=CA:FALSE\nsubjectAltName=DNS:localhost\nextendedKeyUsage={eku}\nkeyUsage=critical,digitalSignature\n"
        );
        put(dir.join("ext"), ext).unwrap();
        let x509 = [
            "x509", "-req", "-in", &csr, "-CA", "ca.pem", "-CAkey", "ca.key", "-extfile", "ext",
            "-out", &cert,
        ];
        ossl(o, dir, &[&x509[..], validity].concat());
    }
}

/// nabi's client against `openssl s_server -rev` with `server` arguments:
/// the client's report.
pub(crate) fn vs_s_server(
    o: &Path,
    probe: &Path,
    dir: &Path,
    server: &str,
    client: &str,
) -> String {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut srv = Command::new(o)
        .current_dir(dir)
        .args([
            "s_server",
            "-accept",
            &port.to_string(),
            "-tls1_3",
            "-rev",
            "-cert",
            "p256.pem",
            "-key",
            "p256.key",
        ])
        .args(server.split_whitespace())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(srv.stdout.take().unwrap()).lines();
    while let Some(Ok(l)) = lines.next() {
        if l.starts_with("ACCEPT") {
            break;
        }
    }
    std::thread::spawn(move || lines.for_each(drop));
    let out = Command::new(probe)
        .current_dir(dir)
        .args(["client", &port.to_string(), "localhost", "ca.pem"])
        .args(client.split_whitespace())
        .output()
        .unwrap();
    let _ = srv.kill();
    let _ = srv.wait();
    text(&out.stdout)
}

/// `openssl s_client` (`args`, once per `conns`, resuming after the
/// first) against nabi's server: per connection what `s_client` saw, then
/// the server's report.
pub(crate) fn vs_s_client(
    o: &Path,
    probe: &Path,
    dir: &Path,
    server: &str,
    args: &str,
    conns: usize,
) -> (Vec<String>, String) {
    let mut srv = Command::new(probe)
        .current_dir(dir)
        .args(["server", "p256.pem", "p256.key", &format!("conns={conns}")])
        .args(server.split_whitespace())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(srv.stderr.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    let port = first.trim().strip_prefix("listening ").unwrap().to_string();
    let mut seen = Vec::new();
    for i in 0..conns {
        let mut c = Command::new(o)
            .current_dir(dir)
            .args([
                "s_client",
                "-connect",
                &format!("127.0.0.1:{port}"),
                "-tls1_3",
                "-servername",
                "localhost",
            ])
            .args([
                "-CAfile",
                "ca.pem",
                "-verify_return_error",
                "-ign_eof",
                "-sess_out",
                "sess.pem",
            ])
            .args(if i > 0 {
                &["-sess_in", "sess.pem"][..]
            } else {
                &[]
            })
            .args(args.split_whitespace())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin.take().unwrap().write_all(b"ping\nbye\n").unwrap();
        let out = c.wait_with_output().unwrap();
        let all = text(&out.stdout) + &text(&out.stderr);
        let facts: Vec<&str> = [
            "echo:ping",
            "Reused,",
            "Negotiated TLS1.3 group: X25519MLKEM768",
        ]
        .into_iter()
        .filter(|k| all.contains(k))
        .collect();
        seen.push(format!(
            "exit {} {}",
            out.status.code().unwrap_or(-1),
            facts.join(" | ")
        ));
    }
    let mut report = String::new();
    srv.stdout
        .take()
        .unwrap()
        .read_to_string(&mut report)
        .unwrap();
    let _ = srv.wait();
    (seen, report)
}

/// `net::tls` against OpenSSL 3 on loopback, both directions. Skipped
/// without OpenSSL 3.
#[test]
fn tls_interop_openssl() {
    let env = setup();
    let Some((o, minor)) = openssl3().filter(|_| crate::common::test_target().is_none()) else {
        let _ = writeln!(
            std::io::stderr(),
            "\n!!!!!!!! SKIPPED tls_interop_openssl: no OpenSSL 3 (set NABI_OPENSSL) !!!!!!!!\n"
        );
        return;
    };
    if minor < 2 {
        let _ = writeln!(
            std::io::stderr(),
            "\n!!!!!!!! SKIPPED tls_interop_openssl: OpenSSL 3.2+ needed (x509 -not_before / -not_after); found 3.{minor} (set NABI_OPENSSL) !!!!!!!!\n"
        );
        return;
    }
    let hybrid = minor >= 5;
    let dir = scratch("tls_interop");
    interop_certs(&o, &dir);
    let probe = dir.join(exe_name("probe"));
    let src = workspace().join("tests/tlsinterop.nabi");
    assert_exit(&crate::common::nabic_build(&env, &src, &probe).unwrap(), 0);

    let ok = "conn 0: resumed false early false reply gnip\n";
    let group = if hybrid { "X25519MLKEM768" } else { "X25519" };
    let cells: [(&str, String, &str); 6] = [
        (
            "aes128 x25519",
            "-ciphersuites TLS_AES_128_GCM_SHA256 -groups X25519".into(),
            "suite=aes128 pq=0",
        ),
        (
            "aes256",
            format!("-ciphersuites TLS_AES_256_GCM_SHA384 -groups {group}"),
            "suite=aes256",
        ),
        (
            "chacha20",
            "-ciphersuites TLS_CHACHA20_POLY1305_SHA256".into(),
            "suite=chacha",
        ),
        (
            "ed25519 server",
            "-cert ed25519.pem -key ed25519.key".into(),
            "",
        ),
        ("stateless HRR", "-stateless".into(), ""),
        (
            "client certificate",
            "-Verify 1 -CAfile ca.pem -verify_return_error".into(),
            "cert=client.pem key=client.key",
        ),
    ];
    for (what, server, client) in &cells {
        assert_eq!(vs_s_server(&o, &probe, &dir, server, client), ok, "{what}");
    }
    assert_eq!(
        vs_s_server(&o, &probe, &dir, "-naccept 2", "conns=2"),
        format!("{ok}conn 1: resumed true early false reply gnip\n"),
        "resumption"
    );
    assert_eq!(
        vs_s_server(&o, &probe, &dir, "-cert expired.pem -key expired.key", ""),
        "conn 0: error Certificate alert 45\n",
        "expired"
    );

    let echo = "exit 0 echo:ping";
    for suite in [
        "TLS_AES_128_GCM_SHA256",
        "TLS_AES_256_GCM_SHA384",
        "TLS_CHACHA20_POLY1305_SHA256",
    ] {
        let (seen, report) = vs_s_client(
            &o,
            &probe,
            &dir,
            "",
            &format!("-ciphersuites {suite} -groups X25519"),
            1,
        );
        assert_eq!(seen, [echo], "{suite}");
        assert_eq!(
            report, "srv 0: resumed false early  peer certs 0\n",
            "{suite}"
        );
    }
    if hybrid {
        // s_client's first share is X25519's; the server asks for the hybrid.
        let (seen, _) = vs_s_client(&o, &probe, &dir, "", "-groups X25519:X25519MLKEM768", 1);
        assert_eq!(
            seen,
            [format!("{echo} | Negotiated TLS1.3 group: X25519MLKEM768")],
            "HelloRetryRequest"
        );
    }
    let (seen, report) = vs_s_client(&o, &probe, &dir, "", "", 2);
    assert!(
        seen[1].starts_with(&format!("{echo} | Reused,")),
        "resumption {seen:?}"
    );
    assert!(
        report.ends_with("srv 1: resumed true early  peer certs 0\n"),
        "{report}"
    );
    let (seen, report) = vs_s_client(
        &o,
        &probe,
        &dir,
        "croots=ca.pem",
        "-cert client.pem -key client.key",
        1,
    );
    assert!(seen[0].starts_with(echo), "client certificate {seen:?}");
    // s_client may send the CA after the leaf.
    assert!(
        report.starts_with("srv 0: resumed false early  peer certs ")
            && !report.contains("certs 0"),
        "{report}"
    );
}
