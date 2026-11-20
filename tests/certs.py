#!/usr/bin/env python3
# Regenerate the X.509 and OCSP fixtures of the net::tls cases and the
# reference signatures of the RSA cases, then patch them into the case files.
#
#   tests/certs.py [workdir]
#
# Needs OpenSSL 3 ($OPENSSL, default the Homebrew openssl@3). Leaf keys
# already in a case are reused so every key keeps matching its certificate;
# CA, intermediate and OCSP responder keys are new on every run.

import base64
import hashlib
import os
import re
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CASES = os.path.join(ROOT, "tests", "cases")
OPENSSL = os.environ.get("OPENSSL", "/opt/homebrew/opt/openssl@3/bin/openssl")
WORK = sys.argv[1] if len(sys.argv) > 1 else tempfile.mkdtemp(prefix="nabi-certs-")
os.makedirs(WORK, exist_ok=True)


def case(name):
    return os.path.join(CASES, f"std_{name}.nabi")


def ssl(*args, data=None):
    r = subprocess.run([OPENSSL, *args], input=data, capture_output=True, cwd=WORK)
    if r.returncode:
        sys.exit(f"openssl {' '.join(args)}: {r.stderr.decode()}")
    return r.stdout


def w(name):
    return os.path.join(WORK, name)


CONST = r'(const {}: str = ")(-----BEGIN ([A-Z ]+)-----\n.*?-----END \3-----\n)(";)'


def read_pem(name, const):
    s = open(case(name)).read()
    m = re.search(CONST.format(const), s, re.S)
    if not m:
        sys.exit(f"{name}: no PEM const {const}")
    return m.group(2)


def to_pem(der, label="CERTIFICATE"):
    b = base64.b64encode(der).decode()
    lines = [b[i:i + 64] for i in range(0, len(b), 64)]
    return f"-----BEGIN {label}-----\n" + "\n".join(lines) + f"\n-----END {label}-----\n"


# Keys. A tuple names a key already in a case; a string is a new key.
KEYS = {
    "test_ca": "ed25519",
    "test_int": "ed25519",
    "tls3_ca": "p256",
    "evil_ca": "p256",
    "resp": "p256",
    "robust_ca": "ed25519",
    "p256": ("302_5_tls_async", "KEY"),
    "ed": ("302_5_tls_chain", "KEY"),
    "p384": ("302_5_tls_server_keys", "P384_KEY"),
    "rsa": ("302_5_tls_server_keys", "RSA_KEY"),
    "srv": ("302_5_tls_ocsp", "KEY"),
    "rsa3": ("302_5_tls_ocsp", "RSA_KEY"),
    "cli": ("302_5_tls_client_auth", "CLI_KEY"),
    "cli2": ("302_5_tls_client_auth", "CLI2_KEY"),
    "pss": ("302_5_tls_pss_keys", "PSS_KEY"),
    "pssany": ("302_5_tls_pss_keys", "PSS_ANY_KEY"),
    "robust": ("302_5_tls_hostile", "KEY"),
}

for k, src in KEYS.items():
    if isinstance(src, tuple):
        open(w(k + ".key"), "w").write(read_pem(*src))
    elif src == "ed25519":
        ssl("genpkey", "-algorithm", "ed25519", "-out", w(k + ".key"))
    else:
        ssl("genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256", "-out", w(k + ".key"))
    ssl("pkey", "-in", w(k + ".key"), "-pubout", "-out", w(k + ".pub"))

# Validity windows.
T1 = ("20261015073607Z", "21260921073607Z")
T2 = ("20261015074900Z", "21260921074900Z")
T3 = ("20261016164528Z", "21260922164528Z")
T3B = ("20261016164529Z", "21260922164529Z")
T4 = ("20260101000000Z", "21260101000000Z")
PAST = ("20200101000000Z", "20210101000000Z")
FUTURE = ("21000101000000Z", "21010101000000Z")

CA_EXT = """subjectKeyIdentifier=hash
authorityKeyIdentifier=keyid:always
basicConstraints=critical,CA:TRUE
keyUsage=critical,keyCertSign"""
LOCAL = "DNS:localhost,DNS:*.nabi.test,IP:127.0.0.1"
TEST_LEAF = f"""subjectAltName={LOCAL}
extendedKeyUsage=serverAuth
keyUsage=critical,digitalSignature
basicConstraints=CA:FALSE
subjectKeyIdentifier=hash
authorityKeyIdentifier=keyid"""


def chain_leaf(san, eku):
    return f"""subjectAltName={san}
extendedKeyUsage={eku}
basicConstraints=CA:FALSE
subjectKeyIdentifier=hash
authorityKeyIdentifier=keyid"""


def tls3_leaf(eku, extra=""):
    return f"""basicConstraints=CA:FALSE
keyUsage=critical,digitalSignature
extendedKeyUsage={eku}
subjectAltName=DNS:localhost,IP:127.0.0.1
{extra}subjectKeyIdentifier=hash
authorityKeyIdentifier=keyid"""


def robust_leaf(san, eku):
    return f"""basicConstraints=CA:FALSE
subjectAltName={san}
extendedKeyUsage={eku}
keyUsage=critical,digitalSignature
subjectKeyIdentifier=hash
authorityKeyIdentifier=keyid"""


# name: (issuer or None for self-signed, key, serial, CN, validity, extensions)
CERTS = {
    "test_ca": (None, "test_ca", "1EE0D099DC8B52F558187B39AC3C35BD853C6C82", "nabi test CA", T1, CA_EXT),
    "test_p256": ("test_ca", "p256", "71E3B0DF5E5252EE4A9D30585FA10463930B4495", "localhost", T1, TEST_LEAF),
    "test_ed": ("test_ca", "ed", "71E3B0DF5E5252EE4A9D30585FA10463930B4494", "localhost", T1, TEST_LEAF),
    "test_p384": ("test_ca", "p384", "71E3B0DF5E5252EE4A9D30585FA10463930B4496", "localhost", T1, TEST_LEAF),
    "test_rsa": ("test_ca", "rsa", "71E3B0DF5E5252EE4A9D30585FA10463930B4497", "localhost", T1, TEST_LEAF),
    "test_int": ("test_ca", "test_int", "71E3B0DF5E5252EE4A9D30585FA10463930B4498", "nabi test intermediate", T2,
                 """basicConstraints=critical,CA:TRUE,pathlen:0
keyUsage=critical,keyCertSign
nameConstraints=critical,permitted;DNS:nabi.test
subjectKeyIdentifier=hash
authorityKeyIdentifier=keyid"""),
    "good": ("test_int", "ed", "25172103B4F8A097109BE7F5AA2F96A9D9888B1A", "good", T2,
             chain_leaf("DNS:www.nabi.test", "serverAuth")),
    "outside": ("test_int", "ed", "25172103B4F8A097109BE7F5AA2F96A9D9888B1B", "outside", T2,
                chain_leaf("DNS:evil.example", "serverAuth")),
    "expired": ("test_int", "ed", "25172103B4F8A097109BE7F5AA2F96A9D9888B1C", "expired", PAST,
                chain_leaf("DNS:www.nabi.test", "serverAuth")),
    "clientonly": ("test_int", "ed", "25172103B4F8A097109BE7F5AA2F96A9D9888B1D", "clientonly", T2,
                   chain_leaf("DNS:www.nabi.test", "clientAuth")),
    "deep": ("good", "ed", "502D6FBB7F940440675922391E356B711773ACE9", "deep", T2,
             """subjectAltName=DNS:deep.nabi.test
extendedKeyUsage=serverAuth
basicConstraints=CA:FALSE
subjectKeyIdentifier=hash
authorityKeyIdentifier=none"""),
    "tls3_ca": (None, "tls3_ca", "7AC49297DF2EACD8D774883D644721697A2C2A7F", "nabi tls3 CA", T3,
                """basicConstraints=critical,CA:TRUE
keyUsage=critical,keyCertSign,cRLSign
subjectKeyIdentifier=hash
authorityKeyIdentifier=none"""),
    "srv": ("tls3_ca", "srv", "35504B558F59459A3061229F3D07F3EE59B2728D", "srv", T3, tls3_leaf("serverAuth")),
    "staple": ("tls3_ca", "srv", "35504B558F59459A3061229F3D07F3EE59B2728E", "staple", T3,
               tls3_leaf("serverAuth", "tlsfeature=status_request\n")),
    "pss": ("tls3_ca", "pss", "35504B558F59459A3061229F3D07F3EE59B2728F", "pss", T3, tls3_leaf("serverAuth")),
    "pssany": ("tls3_ca", "pssany", "35504B558F59459A3061229F3D07F3EE59B27290", "pssany", T3, tls3_leaf("serverAuth")),
    "rsa3": ("tls3_ca", "rsa3", "35504B558F59459A3061229F3D07F3EE59B27291", "rsa", T3, tls3_leaf("serverAuth")),
    "cli": ("tls3_ca", "cli", "35504B558F59459A3061229F3D07F3EE59B27292", "cli", T3, tls3_leaf("clientAuth")),
    "cli2": ("tls3_ca", "cli2", "35504B558F59459A3061229F3D07F3EE59B27293", "cli2", T3, tls3_leaf("clientAuth")),
    "srvonly": ("tls3_ca", "cli", "35504B558F59459A3061229F3D07F3EE59B27294", "srvonly", T3, tls3_leaf("serverAuth")),
    "resp": ("tls3_ca", "resp", "35504B558F59459A3061229F3D07F3EE59B27295", "resp", T3, tls3_leaf("OCSPSigning")),
    "evil_ca": (None, "evil_ca", "75F36765CB274D60DCD1CA07F617BB9DB4604BFA", "evil CA", T3B,
                """basicConstraints=critical,CA:TRUE
keyUsage=critical,keyCertSign,cRLSign
subjectKeyIdentifier=hash
authorityKeyIdentifier=none"""),
    "evil_cli2": ("evil_ca", "cli2", "75F36765CB274D60DCD1CA07F617BB9DB4604BFB", "cli2", T3B, tls3_leaf("clientAuth")),
    "robust_ca": (None, "robust_ca", "4C4C7EE9851301B7537631BCDB4175CA46DA320A", "nabi robust CA", T4, CA_EXT),
    "r_good": ("robust_ca", "robust", "5847B9B3457A56AB96A7BA9385040C669D74B5ED", "localhost", T4,
               robust_leaf("DNS:localhost", "serverAuth")),
    "r_expired": ("robust_ca", "robust", "20B2CF387231832EBE840235AC41508CF4911C10", "localhost", PAST,
                  robust_leaf("DNS:localhost", "serverAuth")),
    "r_future": ("robust_ca", "robust", "5B8D0758B5FE10C7CFBC60442959976F5715848D", "localhost", FUTURE,
                 robust_leaf("DNS:localhost", "serverAuth")),
    "r_client": ("robust_ca", "robust", "4C3839DF7DAAC4D2F21B2B00E855522F088DC4ED", "localhost", T4,
                 robust_leaf("DNS:localhost", "clientAuth")),
    "r_host": ("robust_ca", "robust", "4EA8FAAD865A770699BA094E5CC55A49AA38AE8A", "localhost", T4,
               robust_leaf("DNS:other.example", "serverAuth")),
}

ECDSA = {"tls3_ca", "evil_ca"}

for name, (issuer, key, serial, cn, (nb, na), ext) in CERTS.items():
    open(w(name + ".ext"), "w").write("[x]\n" + ext + "\n")
    args = ["x509", "-new", "-subj", f"/CN={cn}", "-set_serial", "0x" + serial,
            "-not_before", nb, "-not_after", na, "-extfile", w(name + ".ext"), "-extensions", "x",
            "-out", w(name + ".pem")]
    if issuer is None:
        args += ["-key", w(key + ".key")]
    else:
        args += ["-force_pubkey", w(key + ".pub"), "-CA", w(issuer + ".pem"), "-CAkey", w(CERTS[issuer][1] + ".key")]
    if (issuer or name) in ECDSA:
        args += ["-sha256"]
    ssl(*args)


def der(name):
    return ssl("x509", "-in", w(name + ".pem"), "-outform", "DER")


# A signature byte flipped: the certificate no longer verifies.
bad = bytearray(der("r_good"))
bad[-5] ^= 1
BAD_SIG = to_pem(bytes(bad))

# OCSP responses, one request each; the CertID hash follows the request.
open(w("index.txt"), "w").write(
    f"V\t{T3[1][2:]}\t\t{CERTS['srv'][2]}\tunknown\t/CN=srv\n"
    f"V\t{T3[1][2:]}\t\t{CERTS['staple'][2]}\tunknown\t/CN=staple\n"
    f"R\t{T3[1][2:]}\t260101000000Z\t{CERTS['rsa3'][2]}\tunknown\t/CN=rsa\n")


def ocsp(cert, signer="tls3_ca", md="-sha1"):
    req = w(f"{cert}-{signer}{md}.req")
    resp = w(f"{cert}-{signer}{md}.resp")
    ssl("ocsp", "-issuer", w("tls3_ca.pem"), md, "-cert", w(cert + ".pem"), "-no_nonce", "-reqout", req)
    ssl("ocsp", "-index", w("index.txt"), "-CA", w("tls3_ca.pem"), "-rsigner", w(signer + ".pem"),
        "-rkey", w(CERTS[signer][1] + ".key"), "-reqin", req, "-respout", resp, "-ndays", "36500")
    return open(resp, "rb").read()


OCSP = {
    "GOOD": ocsp("srv"),
    "REVOKED": ocsp("rsa3"),
    "DELEGATED": ocsp("srv", "resp"),
    "SHA256_ID": ocsp("srv", md="-sha256"),
    "OTHER": ocsp("rsa3"),
    "STAPLE": ocsp("staple"),
}
# The case flips byte 55 and expects a bad signature: keep it in the name.
at = OCSP["GOOD"].find(b"nabi tls3 CA")
assert at <= 55 < at + 12, at

# Certificate -> the case constants that hold it.
TARGETS = {
    "test_ca": ["async:CA", "chain:CA", "config_new:CA", "loopback:CA", "reader_writer:CA",
                "server_keys:CA", "split_threads:CA"],
    "test_p256": ["async:CERT", "reader_writer:CERT", "server_keys:P256_CERT", "split_threads:CERT"],
    "test_ed": ["loopback:LEAF"],
    "test_p384": ["server_keys:P384_CERT"],
    "test_rsa": ["server_keys:RSA_CERT"],
    "test_int": ["chain:INT"],
    "good": ["chain:GOOD"],
    "outside": ["chain:OUTSIDE"],
    "expired": ["chain:EXPIRED"],
    "clientonly": ["chain:CLIENTONLY"],
    "deep": ["chain:DEEP"],
    "tls3_ca": ["client_auth:CA", "early_async:CA", "ocsp:CA", "pss_keys:CA", "resumption:CA"],
    "srv": ["client_auth:LEAF", "early_async:LEAF", "ocsp:LEAF", "pss_keys:EC", "resumption:LEAF"],
    "staple": ["ocsp:STAPLE_LEAF"],
    "pss": ["pss_keys:PSS"],
    "pssany": ["pss_keys:PSS_ANY"],
    "rsa3": ["ocsp:RSA_LEAF", "pss_keys:RSA"],
    "cli": ["client_auth:CLI"],
    "cli2": ["client_auth:CLI2"],
    "srvonly": ["client_auth:SERVER_ONLY"],
    "evil_cli2": ["client_auth:OTHER_CA_CLI"],
    "robust_ca": ["hostile:CA"],
    "r_good": ["hostile:GOOD"],
    "r_expired": ["hostile:EXPIRED"],
    "r_future": ["hostile:FUTURE"],
    "r_client": ["hostile:CLIENT_ONLY"],
    "r_host": ["hostile:WRONG_HOST"],
}

texts = {}


def text(name):
    if name not in texts:
        texts[name] = open(case(name)).read()
    return texts[name]


def put_pem(name, const, pem):
    s, n = re.subn(CONST.format(const), lambda m: m.group(1) + pem + m.group(4), text(name), flags=re.S)
    assert n == 1, (name, const)
    texts[name] = s


def put_hex(name, const, blob):
    s, n = re.subn(rf'(const {const}: str = ")[0-9a-f]*(";)', lambda m: m.group(1) + blob.hex() + m.group(2),
                   text(name), flags=re.S)
    assert n == 1, (name, const)
    texts[name] = s


count = 0
for cert, targets in TARGETS.items():
    pem = to_pem(der(cert))
    for t in targets:
        c, const = t.split(":")
        put_pem("302_5_tls_" + c, const, pem)
        count += 1
put_pem("302_5_tls_hostile", "BAD_SIG", BAD_SIG)
count += 1
for const, blob in OCSP.items():
    put_hex("302_5_tls_ocsp", const, blob)

# RSA reference signatures, computed here: PSS needs the case's own salts.


def der_ints(b):
    # PKCS #1 RSAPrivateKey: a SEQUENCE of INTEGERs.
    def tlv(i):
        t, n = b[i], b[i + 1]
        i += 2
        if n & 0x80:
            k = n & 0x7f
            n = int.from_bytes(b[i:i + k], "big")
            i += k
        return t, b[i:i + n], i + n
    _, body, _ = tlv(0)
    b, i, out = body, 0, []
    while i < len(b):
        _, v, i = tlv(i)
        out.append(int.from_bytes(v, "big"))
    return out


DIGEST = {
    "sha256": bytes.fromhex("3031300d060960864801650304020105000420"),
    "sha384": bytes.fromhex("3041300d060960864801650304020205000430"),
    "sha512": bytes.fromhex("3051300d060960864801650304020305000440"),
}


def mgf1(h, seed, n):
    out = b""
    for c in range((n + hashlib.new(h).digest_size - 1) // hashlib.new(h).digest_size):
        out += hashlib.new(h, seed + c.to_bytes(4, "big")).digest()
    return out[:n]


def rsa_sign(key, h, msg, salt=None):
    _, n, _, d = der_ints(key)[:4]
    k = (n.bit_length() + 7) // 8
    mh = hashlib.new(h, msg).digest()
    if salt is None:
        t = DIGEST[h] + mh
        em = b"\x00\x01" + b"\xff" * (k - len(t) - 3) + b"\x00" + t
    else:
        bits = n.bit_length() - 1
        el = (bits + 7) // 8
        hh = hashlib.new(h, b"\x00" * 8 + mh + salt).digest()
        db = b"\x00" * (el - len(salt) - len(hh) - 2) + b"\x01" + salt
        mask = bytearray(x ^ y for x, y in zip(db, mgf1(h, hh, len(db))))
        mask[0] &= 0xff >> (8 * el - bits)
        em = bytes(mask) + hh + b"\xbc"
    return pow(int.from_bytes(em, "big"), d, n).to_bytes(k, "big")


name = "221_crypto_rsa_sign"
s = text(name)
assert 'let msg = f"nabi pss {f[0]}";' in s
for m in list(re.finditer(r'"(rsa\d+) (sha\d+) ([0-9a-f]+) ([0-9a-f]+) ([0-9a-f]+)"', s)):
    sig = rsa_sign(bytes.fromhex(m.group(3)), m.group(2), f"nabi pss {m.group(1)}".encode(), bytes.fromhex(m.group(4)))
    s = s.replace(m.group(0), m.group(0)[:m.start(5) - m.start(0)] + sig.hex() + '"')
texts[name] = s

name = "221_crypto_rsa_pkcs1_sign"
s = text(name)
msg = b"nabi pkcs1 message"
assert 'let msg = "nabi pkcs1 message";' in s
keys = {k: bytes.fromhex(re.search(rf'const {k.upper()}: str = "([0-9a-f]+)";', s).group(1)) for k in ("k2048", "k2049")}
for m in list(re.finditer(r'"(rsa\d+) (sha\d+) ([0-9a-f]+)"', s)):
    sig = rsa_sign(keys["k" + m.group(1)[3:]], m.group(2), msg)
    s = s.replace(m.group(0), f'"{m.group(1)} {m.group(2)} {sig.hex()}"')
for m in list(re.finditer(r'"(\d+) ([0-9a-f]+)"', s)):
    n = int(m.group(1))
    sig = rsa_sign(keys["k2048"], "sha256", msg, bytes((7 * i + n) & 0xff for i in range(n)))
    s = s.replace(m.group(0), f'"{n} {sig.hex()}"')
texts[name] = s

for name, s in texts.items():
    open(case(name), "w").write(s)
print(f"{WORK}: {count} certificates, {len(OCSP)} OCSP responses, {len(texts)} cases patched")
