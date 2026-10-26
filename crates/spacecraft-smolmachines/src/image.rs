//! Resolution of an image to its digest: a `docker save` archive through its `manifest.json`,
//! a registry reference through the digest it pins, or else through its registry.

use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, WWW_AUTHENTICATE};
use serde_json::Value;
use std::process::Command;
use std::time::Duration;

use spacecraft::image::{Archive, Digest, Image, Reference};
use spacecraft::{Error, Result};

/// The host tool that reads `manifest.json` out of an archive.
const TAR: &str = "tar";

/// `tar` flags: extract (`x`) one member to stdout (`O`) from the archive file (`f`).
const TAR_EXTRACT_TO_STDOUT: &str = "-xOf";

/// The archive member in which `docker save` names the image's config file.
const MANIFEST: &str = "manifest.json";

/// The key, in each `manifest.json` entry, of the config file's path.
const CONFIG_KEY: &str = "Config";

/// The directory where newer `docker save` stores the config file, named by its SHA-256 hex.
const OCI_CONFIG_DIR: &str = "blobs/sha256/";

/// The extension older `docker save` gives the config file, named by its SHA-256 hex.
const LEGACY_CONFIG_EXTENSION: &str = ".json";

/// The scheme of a registry's URL.
const HTTPS: &str = "https://";

/// How long each request of a registry lookup may take, from connecting to the end of the reply.
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(30);

/// The host by which references name Docker Hub.
const DOCKER_HUB: &str = "docker.io";

/// The host that serves Docker Hub's registry API: `docker.io` redirects to the website.
const DOCKER_HUB_REGISTRY: &str = "registry-1.docker.io";

/// The namespace of Docker Hub's official images, which a reference may leave out, as
/// `docker.io/alpine` does.
const DOCKER_HUB_OFFICIAL: &str = "library/";

/// Separates the segments of a repository path.
const PATH_SEPARATOR: char = '/';

/// What comes between the registry and the repository path in a manifest's URL.
const API_PREFIX: &str = "/v2/";

/// What comes between the repository path and the tag in a manifest's URL.
const MANIFESTS: &str = "/manifests/";

/// The manifest kinds the client accepts, as OCI and Docker name them. Indexes are among them,
/// so a multi-platform tag answers with the digest of its index.
const MEDIA_TYPES: &str = "application/vnd.oci.image.index.v1+json, \
    application/vnd.docker.distribution.manifest.list.v2+json, \
    application/vnd.oci.image.manifest.v1+json, \
    application/vnd.docker.distribution.manifest.v2+json";

/// The header in which a registry names the digest of the manifest it answers with.
const DOCKER_CONTENT_DIGEST: &str = "Docker-Content-Digest";

/// The scheme of a challenge that asks for a bearer token, with the space that ends it.
const BEARER: &str = "Bearer ";

/// Separates the parameters of a challenge.
const PARAM_SEPARATOR: char = ',';

/// Separates a challenge parameter's name from its value.
const VALUE_SEPARATOR: char = '=';

/// Wraps a challenge parameter's value.
const VALUE_QUOTE: char = '"';

/// The challenge parameter that names the token endpoint.
const REALM: &str = "realm";

/// The challenge parameter that names the registry's service, passed on to the token endpoint.
const SERVICE: &str = "service";

/// The challenge parameter that names the access asked for, passed on to the token endpoint.
const SCOPE: &str = "scope";

/// The key of the token in the token endpoint's reply.
const TOKEN: &str = "token";

/// The other key under which a token endpoint may reply with the token.
const ACCESS_TOKEN: &str = "access_token";

/// Separates an error from its cause in a message.
const CAUSE_SEPARATOR: &str = ": ";

/// Resolves `image` to the digest of its content: an archive through its `manifest.json`, a
/// registry reference through the digest it pins, or else by asking its registry.
pub(crate) fn digest(image: &Image) -> Result<Digest> {
    match image {
        Image::Archive(archive) => archive_digest(archive),
        Image::Registry(reference) => {
            if let Some(digest) = reference.digest() {
                return Ok(digest.clone());
            }

            let client = Client::builder()
                .timeout(REGISTRY_TIMEOUT)
                .build()
                .map_err(|error| {
                    let message =
                        format!("the HTTP client could not start: {}", request_error(error));
                    unresolved(reference.as_str(), message)
                })?;
            // References name Docker Hub `docker.io`, which serves no registry API.
            let host = if reference.host() == DOCKER_HUB {
                DOCKER_HUB_REGISTRY
            } else {
                reference.host()
            };
            registry_digest(&client, &format!("{HTTPS}{host}"), reference)
        }
    }
}

/// Reads the digest of `archive` from its `manifest.json`.
fn archive_digest(archive: &Archive) -> Result<Digest> {
    let path = archive.as_str();
    let output = Command::new(TAR)
        .arg(TAR_EXTRACT_TO_STDOUT)
        .arg(path)
        .arg(MANIFEST)
        .output()
        .map_err(|error| unresolved(path, format!("tar could not run: {error}")))?;
    // bsdtar can warn on stderr and still succeed, so the status alone decides.
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(unresolved(
            path,
            format!("tar -xOf failed ({}): {}", output.status, stderr.trim_end()),
        ));
    }

    manifest_digest(path, &String::from_utf8_lossy(&output.stdout))
}

/// Reads the image's digest from a `docker save` manifest: the hex by which its first entry's
/// `Config` path names the config file.
fn manifest_digest(image: &str, manifest: &str) -> Result<Digest> {
    let entries: Value = serde_json::from_str(manifest)
        .map_err(|error| unresolved(image, format!("manifest.json is not JSON: {error}")))?;
    let config = entries
        .get(0)
        .and_then(|entry| entry.get(CONFIG_KEY))
        .and_then(Value::as_str)
        .ok_or_else(|| unresolved(image, "manifest.json has no Config entry".to_string()))?;

    config_hex(config)
        .and_then(|hex| Digest::from_hex(hex).ok())
        .ok_or_else(|| {
            unresolved(
                image,
                format!("Config names {config}, which is no sha256 hex"),
            )
        })
}

/// The hex by which a `Config` path names the config file: after `OCI_CONFIG_DIR`, or before
/// `LEGACY_CONFIG_EXTENSION`.
fn config_hex(config: &str) -> Option<&str> {
    config
        .strip_prefix(OCI_CONFIG_DIR)
        .or_else(|| config.strip_suffix(LEGACY_CONFIG_EXTENSION))
}

/// Asks the registry at `base` for the digest of `reference`'s tag: a `HEAD` on its manifest,
/// and, when the registry asks for a bearer token, an anonymous token and the `HEAD` again.
///
/// ponytail: anonymous only, enough for public images. `oci-client` takes over when logins to
/// private registries or registry mirrors arrive.
fn registry_digest(client: &Client, base: &str, reference: &Reference) -> Result<Digest> {
    let image = reference.as_str();
    let path = reference.path();
    // Docker Hub keeps its official images under `library/`, which a reference may leave out.
    let path = if reference.host() == DOCKER_HUB && !path.contains(PATH_SEPARATOR) {
        format!("{DOCKER_HUB_OFFICIAL}{path}")
    } else {
        path.to_string()
    };
    let tag = reference.tag();
    let url = format!("{base}{API_PREFIX}{path}{MANIFESTS}{tag}");
    let head = |token: Option<&str>| {
        let mut request = client.head(&url).header(ACCEPT, MEDIA_TYPES);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request
            .send()
            .map_err(|error| unresolved(image, format!("{} from {url}", request_error(error))))
    };

    let mut response = head(None)?;
    if response.status() == StatusCode::UNAUTHORIZED {
        let challenge = response
            .headers()
            .get(WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .and_then(bearer_challenge)
            .ok_or_else(|| {
                unresolved(image, format!("401 without a Bearer challenge from {url}"))
            })?;
        let token = anonymous_token(client, image, &challenge)?;
        response = head(Some(&token))?;
    }
    // A private image ends here, with 401 or 403.
    if response.status() != StatusCode::OK {
        return Err(unresolved(
            image,
            format!("{} from {url}", response.status()),
        ));
    }

    let value = response
        .headers()
        .get(DOCKER_CONTENT_DIGEST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| unresolved(image, format!("no Docker-Content-Digest header from {url}")))?;
    Digest::parse(value).map_err(|_| {
        unresolved(
            image,
            format!("malformed Docker-Content-Digest {value} from {url}"),
        )
    })
}

/// Asks the token endpoint that `challenge` names for a token, without credentials.
fn anonymous_token(client: &Client, image: &str, challenge: &BearerChallenge) -> Result<String> {
    let realm = &challenge.realm;
    // `query` leaves out a parameter whose value is `None`.
    let response = client
        .get(realm)
        .query(&[(SERVICE, &challenge.service), (SCOPE, &challenge.scope)])
        .send()
        .map_err(|error| unresolved(image, format!("{} from {realm}", request_error(error))))?;
    // `text` consumes the response, so the status comes first.
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| unresolved(image, format!("{} from {realm}", request_error(error))))?;
    if status != StatusCode::OK {
        // ghcr.io ends its body with a newline, which the message drops.
        let body = body.trim_end();
        return Err(unresolved(image, format!("{status} from {realm}: {body}")));
    }

    serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|reply| {
            reply
                .get(TOKEN)
                .or_else(|| reply.get(ACCESS_TOKEN))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .ok_or_else(|| unresolved(image, format!("no token in the reply from {realm}")))
}

/// A `WWW-Authenticate` challenge that asks for a bearer token.
#[derive(Debug, PartialEq)]
struct BearerChallenge {
    /// The URL to which the client should send a token request.
    realm: String,
    /// The service name that the client should include in its token request.
    service: Option<String>,
    /// The scope that the client should include in its token request.
    scope: Option<String>,
}

/// Reads a bearer challenge from a `WWW-Authenticate` header: `None` for another scheme, or for
/// a challenge without a realm.
///
/// ponytail: splits on `,` and ignores quoting, enough for the one challenge a registry sends an
/// anonymous pull. A quoted comma, as in `scope="repository:org/name:pull,push"`, cuts the value
/// short, and several challenges in one header read as one. `http-auth` parses the full syntax
/// if a registry ever needs it.
fn bearer_challenge(header: &str) -> Option<BearerChallenge> {
    let header = header.strip_prefix(BEARER)?;
    let mut realm = None;
    let mut service = None;
    let mut scope = None;
    for pair in header.split(PARAM_SEPARATOR) {
        // A pair without a value, such as what follows a quoted comma, is skipped.
        let Some((key, value)) = pair.split_once(VALUE_SEPARATOR) else {
            continue;
        };
        let value = value.trim_matches(VALUE_QUOTE);
        // A space may follow each comma.
        match key.trim() {
            REALM => realm = Some(value.to_string()),
            SERVICE => service = Some(value.to_string()),
            SCOPE => scope = Some(value.to_string()),
            _ => {}
        }
    }
    Some(BearerChallenge {
        realm: realm?,
        service,
        scope,
    })
}

/// Writes a failed request's error with its causes, which its own message leaves out.
fn request_error(error: reqwest::Error) -> String {
    // The caller names the URL, once.
    let error = error.without_url();
    std::iter::successors(Some(&error as &dyn std::error::Error), |error| {
        error.source()
    })
    .map(ToString::to_string)
    .collect::<Vec<_>>()
    .join(CAUSE_SEPARATOR)
}

/// Names the image, and the step of its resolution that failed.
fn unresolved(image: &str, message: String) -> Error {
    Error::ImageResolve {
        image: image.to_string(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[test]
    fn resolves_an_archive_to_its_image_id() {
        let hex = "a".repeat(64);
        let tempdir = tempfile::tempdir().unwrap();
        let manifest = format!(r#"[{{"Config":"blobs/sha256/{hex}"}}]"#);
        let path = archive(tempdir.path(), Some(&manifest));
        let image = Image::parse(path.to_str().unwrap()).unwrap();

        let digest = digest(&image).unwrap();

        assert_eq!(digest.to_string(), format!("sha256:{hex}"));
    }

    #[test]
    fn an_archive_without_a_manifest_is_refused() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = archive(tempdir.path(), None);
        let image = Image::parse(path.to_str().unwrap()).unwrap();

        let error = digest(&image).unwrap_err();

        // GNU tar and bsdtar word their errors differently; both name the missing member.
        assert!(
            matches!(&error, Error::ImageResolve { message, .. }
                if message.contains("manifest.json")),
            "{error}"
        );
    }

    #[test]
    fn a_pinned_reference_needs_no_request() {
        let hex = "a".repeat(64);
        // Nothing listens on port 1, so a request would fail the test.
        let image = Image::parse(&format!("127.0.0.1:1/org/name@sha256:{hex}")).unwrap();

        let digest = digest(&image).unwrap();

        assert_eq!(digest.to_string(), format!("sha256:{hex}"));
    }

    #[test]
    fn resolves_a_tag_through_the_token_dance() {
        let fake = FakeRegistry::start(&format!("sha256:{}", "b".repeat(64)));
        let base = format!("http://127.0.0.1:{}", fake.port);
        let reference = reference(&format!("127.0.0.1:{}/org/name:latest", fake.port));

        let digest = registry_digest(&Client::new(), &base, &reference).unwrap();

        assert_eq!(digest.to_string(), format!("sha256:{}", "b".repeat(64)));
        // One token request, which carried no credentials.
        assert_eq!(*fake.token_requests.lock().unwrap(), vec![None]);
    }

    #[test]
    fn a_tag_is_resolved_over_https() {
        let fake = FakeRegistry::start(&format!("sha256:{}", "b".repeat(64)));
        let image = Image::parse(&format!("127.0.0.1:{}/org/name:latest", fake.port)).unwrap();

        let error = digest(&image).unwrap_err();

        // The fake speaks plain HTTP, so the TLS handshake fails before any request.
        let url = format!(
            "https://127.0.0.1:{}/v2/org/name/manifests/latest",
            fake.port
        );
        assert!(
            matches!(&error, Error::ImageResolve { message, .. }
                if message.ends_with(&format!(" from {url}"))),
            "{error}"
        );
        assert!(fake.token_requests.lock().unwrap().is_empty());
    }

    #[test]
    fn a_missing_repository_is_reported_with_its_status() {
        let fake = FakeRegistry::start(&format!("sha256:{}", "b".repeat(64)));
        let base = format!("http://127.0.0.1:{}", fake.port);
        let reference = reference(&format!("127.0.0.1:{}/org/other:latest", fake.port));

        let error = registry_digest(&Client::new(), &base, &reference).unwrap_err();

        let expected = format!("404 Not Found from {base}/v2/org/other/manifests/latest");
        assert!(
            matches!(&error, Error::ImageResolve { message, .. } if *message == expected),
            "{error}"
        );
    }

    #[test]
    fn a_docker_hub_official_image_is_read_from_library() {
        let fake = FakeRegistry::start(&format!("sha256:{}", "b".repeat(64)));
        let base = format!("http://127.0.0.1:{}", fake.port);
        let reference = reference("docker.io/name:latest");

        let error = registry_digest(&Client::new(), &base, &reference).unwrap_err();

        // The fake serves `org/name` alone, so its 404 names the path the client asked for.
        let expected = format!("404 Not Found from {base}/v2/library/name/manifests/latest");
        assert!(
            matches!(&error, Error::ImageResolve { message, .. } if *message == expected),
            "{error}"
        );
    }

    #[test]
    fn manifest_digest_reads_both_spellings() {
        let hex = "a".repeat(64);
        for manifest in [
            format!(r#"[{{"Config":"blobs/sha256/{hex}"}}]"#),
            format!(r#"[{{"Config":"{hex}.json"}}]"#),
        ] {
            let digest = manifest_digest("/abs/guest.tar", &manifest).unwrap();

            assert_eq!(digest.to_string(), format!("sha256:{hex}"), "{manifest}");
        }
    }

    #[test]
    fn manifest_digest_refuses_malformed_manifests() {
        for (manifest, expected) in [
            ("[]", "manifest.json has no Config entry"),
            ("[{}]", "manifest.json has no Config entry"),
            (
                r#"[{"Config":"blobs/sha256/short"}]"#,
                "Config names blobs/sha256/short, which is no sha256 hex",
            ),
        ] {
            let error = manifest_digest("/abs/guest.tar", manifest).unwrap_err();
            assert!(
                matches!(error, Error::ImageResolve { image, message }
                    if image == "/abs/guest.tar" && message == expected),
                "{manifest}"
            );
        }
    }

    #[test]
    fn manifest_digest_refuses_invalid_json() {
        let error = manifest_digest("/abs/guest.tar", "not json").unwrap_err();

        // serde_json writes the rest, naming the line and column.
        assert!(
            matches!(&error, Error::ImageResolve { message, .. }
                if message.starts_with("manifest.json is not JSON: ")),
            "{error}"
        );
    }

    #[test]
    fn parses_a_bearer_challenge() {
        assert_eq!(
            bearer_challenge(
                r#"Bearer realm="http://127.0.0.1:1/token",service="fake",scope="repository:org/name:pull""#
            ),
            Some(BearerChallenge {
                realm: "http://127.0.0.1:1/token".to_string(),
                service: Some("fake".to_string()),
                scope: Some("repository:org/name:pull".to_string()),
            })
        );
        // A space after a comma is read past. A quoted comma cuts the scope short, and keeps the
        // `pull` an anonymous request asks for.
        assert_eq!(
            bearer_challenge(r#"Bearer realm="r", scope="repository:org/name:pull,push""#),
            Some(BearerChallenge {
                realm: "r".to_string(),
                service: None,
                scope: Some("repository:org/name:pull".to_string()),
            })
        );
        // Service and scope are optional. Another scheme is a challenge this client cannot
        // answer.
        assert_eq!(
            bearer_challenge(r#"Bearer realm="r""#),
            Some(BearerChallenge {
                realm: "r".to_string(),
                service: None,
                scope: None,
            })
        );
        assert_eq!(bearer_challenge(r#"Basic realm="x""#), None);
    }

    /// Writes `other.txt`, and `manifest.json` when given, into `dir`, then archives them as
    /// `dir/guest.tar`.
    fn archive(dir: &Path, manifest: Option<&str>) -> PathBuf {
        // tar refuses to write an empty archive, so `other.txt` is always there.
        let mut members = vec!["other.txt"];
        fs::write(dir.join("other.txt"), "other").unwrap();
        if let Some(manifest) = manifest {
            fs::write(dir.join("manifest.json"), manifest).unwrap();
            members.push("manifest.json");
        }

        // Members are named one by one: archiving `.` stores `./manifest.json`, which GNU tar
        // does not find when asked for `manifest.json`.
        let path = dir.join("guest.tar");
        let status = Command::new("tar")
            .arg("-cf")
            .arg(&path)
            .arg("-C")
            .arg(dir)
            .args(members)
            .status()
            .unwrap();
        assert!(status.success(), "tar exited with {status}");

        path
    }

    /// Takes the registry reference out of `image`, as `digest` does: `Reference::parse` is
    /// core's own.
    fn reference(image: &str) -> Reference {
        let Image::Registry(reference) = Image::parse(image).unwrap() else {
            panic!("{image} is no registry reference");
        };
        reference
    }

    /// A registry on `127.0.0.1` that serves `org/name:latest` behind an anonymous token, and
    /// records the `Authorization` header of every token request.
    struct FakeRegistry {
        port: u16,
        token_requests: Arc<Mutex<Vec<Option<String>>>>,
    }

    impl FakeRegistry {
        /// Starts the registry on a free port, answering `digest` for `org/name:latest`. Its
        /// thread runs until the test process ends.
        fn start(digest: &str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let token_requests = Arc::new(Mutex::new(Vec::new()));

            thread::spawn({
                let digest = digest.to_string();
                let token_requests = Arc::clone(&token_requests);
                move || {
                    // Every answer closes its connection, so a connection carries one request.
                    for mut stream in listener.incoming().flatten() {
                        serve(&mut stream, port, &digest, &token_requests);
                    }
                }
            });

            Self {
                port,
                token_requests,
            }
        }
    }

    /// Answers the request on `stream` as the fake registry on `port`.
    fn serve(
        stream: &mut TcpStream,
        port: u16,
        digest: &str,
        token_requests: &Mutex<Vec<Option<String>>>,
    ) {
        let mut reader = BufReader::new(&mut *stream);

        // Read as bytes, so a TLS hello reads without a UTF-8 error. Returning on it closes the
        // connection, and the client's handshake fails at once.
        let mut request_line = Vec::new();
        if reader.read_until(b'\n', &mut request_line).is_err() {
            return;
        }
        let request_line = String::from_utf8_lossy(&request_line);
        let Some(request) = request_line.strip_suffix(" HTTP/1.1\r\n") else {
            return;
        };

        let mut authorization = None;
        loop {
            let mut line = Vec::new();
            match reader.read_until(b'\n', &mut line) {
                // The client hung up before the blank line that ends the headers.
                Ok(0) | Err(_) => return,
                Ok(_) if line == b"\r\n" => break,
                Ok(_) => {
                    // reqwest writes header names in lowercase.
                    let line = String::from_utf8_lossy(&line);
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("authorization")
                    {
                        authorization = Some(value.trim().to_string());
                    }
                }
            }
        }

        let response = match (request, authorization.as_deref()) {
            ("HEAD /v2/org/name/manifests/latest", None) => format!(
                "HTTP/1.1 401 Unauthorized\r\n\
                 WWW-Authenticate: Bearer realm=\"http://127.0.0.1:{port}/token\",\
                 service=\"fake\",scope=\"repository:org/name:pull\"\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
            ),
            ("HEAD /v2/org/name/manifests/latest", Some("Bearer t0k")) => format!(
                "HTTP/1.1 200 OK\r\nDocker-Content-Digest: {digest}\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
            ),
            (_, authorization) if request.starts_with("GET /token?") => {
                token_requests
                    .lock()
                    .unwrap()
                    .push(authorization.map(str::to_string));
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\n\
                 Connection: close\r\n\r\n{\"token\":\"t0k\"}"
                    .to_string()
            }
            _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_string(),
        };
        // The client may be gone already; the thread lives on for the next connection.
        let _ = stream.write_all(response.as_bytes());
    }
}
