//! Чесна перевірка TLS принтера: ланцюжок до BBL CA і `CN == серійний номер`.
//!
//! Чому власний верифікатор, а не `WebPkiServerVerifier`:
//! - з’єднуємось за IP, а сертифікат виданий на серійний номер (`CN=<SN>`, без SAN) —
//!   стандартна перевірка імені завжди провалиться;
//! - листовий сертифікат A1 (перевірено наживо, прошивка 01.04.60.02) — **X.509 v1**, а webpki
//!   приймає лише v3 (`UnsupportedCertVersion`). Тому розбір — `x509-parser`, а підписи
//!   (і ланцюжка, і рукостискання) перевіряють ті самі алгоритми ring, що й у rustls.
//!
//! Перевірка нічого не пропускає: підпис кожної ланки, термін дії, `CA:TRUE` у проміжних,
//! збіг серійного номера. Якщо модель видає сертифікат не від цього CA — з’єднання
//! розривається з `TlsProblem::UnknownIssuer`, і це видно в помилці, а не ховається.
use std::{
    fmt,
    sync::{Arc, Mutex, OnceLock},
};

use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, WebPkiSupportedAlgorithms},
    pki_types::{CertificateDer, ServerName, UnixTime, pem::PemObject as _},
};
use webpki::ring as alg;
use x509_parser::{certificate::X509Certificate, oid_registry as oid, prelude::FromDer as _};

/// Кореневі й проміжні CA Bambu з `resources/cert/printer.cer` Bambu Studio (публічні):
/// BBL CA (до 2032), BBL CA2 RSA / ECC (самопідписані до 2050 і крос-підписані BBL CA).
pub const BBL_CA_PEM: &[u8] = include_bytes!("../assets/bbl-ca.pem");

/// Найбільша глибина ланцюжка (лист → проміжні → корінь).
const MAX_DEPTH: usize = 4;

/// Чому сертифікат відхилено; зберігається у [`ProblemSlot`], щоб викликач міг показати
/// точну причину навіть крізь шари rumqttc / власного FTPS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TlsProblem {
    BadEncoding,
    NotYetValid,
    Expired,
    /// Жодна ланка не підписана довіреним CA Bambu.
    UnknownIssuer,
    /// Проміжний сертифікат не є CA.
    NotCa,
    UnsupportedSignature,
    SerialMismatch,
}

impl fmt::Display for TlsProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BadEncoding => "пошкоджений сертифікат",
            Self::NotYetValid => "сертифікат ще не дійсний (перевір годинник)",
            Self::Expired => "сертифікат прострочений",
            Self::UnknownIssuer => "ланцюжок не веде до BBL CA",
            Self::NotCa => "проміжний сертифікат не є CA",
            Self::UnsupportedSignature => "непідтримуваний алгоритм підпису",
            Self::SerialMismatch => "CN сертифіката не збігається із серійним номером",
        })
    }
}

/// Остання причина відмови верифікатора для цього з’єднання.
#[derive(Clone, Default, Debug)]
pub struct ProblemSlot(Arc<Mutex<Option<TlsProblem>>>);
impl ProblemSlot {
    pub fn take(&self) -> Option<TlsProblem> {
        self.0.lock().ok()?.take()
    }
    fn set(&self, problem: TlsProblem) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(problem);
        }
    }
}

/// Проміжні CA, що вже пройшли перевірку до довіреного кореня. Довіри не додають: кожне
/// наступне використання знову перевіряє підписи й терміни.
#[derive(Clone, Default)]
struct IntermediateCache(Arc<Mutex<Vec<CertificateDer<'static>>>>);
impl IntermediateCache {
    const LIMIT: usize = 32;
    fn snapshot(&self) -> Vec<CertificateDer<'static>> {
        self.0.lock().map(|c| c.clone()).unwrap_or_default()
    }
    fn remember(&self, certs: impl Iterator<Item = CertificateDer<'static>>) {
        let Ok(mut cache) = self.0.lock() else { return };
        for cert in certs {
            if !cache.contains(&cert) && cache.len() < Self::LIMIT {
                cache.push(cert);
            }
        }
    }
}

fn shared_cache() -> IntermediateCache {
    static CACHE: OnceLock<IntermediateCache> = OnceLock::new();
    CACHE.get_or_init(IntermediateCache::default).clone()
}

struct Anchor {
    /// Біти subjectPublicKey (для RSA — RSAPublicKey DER, для EC — точка).
    key: Vec<u8>,
}

fn parse_pem(pem: &[u8]) -> Vec<CertificateDer<'static>> {
    CertificateDer::pem_slice_iter(pem)
        .filter_map(Result::ok)
        .collect()
}

fn anchors_from(certs: &[CertificateDer<'_>]) -> Vec<Anchor> {
    certs
        .iter()
        .filter_map(|der| {
            let (_, cert) = X509Certificate::from_der(der).ok()?;
            Some(Anchor {
                key: cert.public_key().subject_public_key.data.to_vec(),
            })
        })
        .collect()
}

fn bundled() -> Arc<Vec<Anchor>> {
    static ANCHORS: OnceLock<Arc<Vec<Anchor>>> = OnceLock::new();
    ANCHORS
        .get_or_init(|| Arc::new(anchors_from(&parse_pem(BBL_CA_PEM))))
        .clone()
}

fn provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    PROVIDER
        .get_or_init(|| Arc::new(rustls::crypto::ring::default_provider()))
        .clone()
}

/// Алгоритми ring для підпису сертифіката за OID. Для ECDSA крива визначається ключем
/// підписанта, тож пробуємо обидві.
fn cert_algorithms(
    sig: &x509_parser::der_parser::oid::Oid<'_>,
) -> Vec<&'static dyn rustls::pki_types::SignatureVerificationAlgorithm> {
    if *sig == oid::OID_PKCS1_SHA256WITHRSA {
        vec![alg::RSA_PKCS1_2048_8192_SHA256]
    } else if *sig == oid::OID_PKCS1_SHA384WITHRSA {
        vec![alg::RSA_PKCS1_2048_8192_SHA384]
    } else if *sig == oid::OID_PKCS1_SHA512WITHRSA {
        vec![alg::RSA_PKCS1_2048_8192_SHA512]
    } else if *sig == oid::OID_SIG_ECDSA_WITH_SHA256 {
        vec![alg::ECDSA_P256_SHA256, alg::ECDSA_P384_SHA256]
    } else if *sig == oid::OID_SIG_ECDSA_WITH_SHA384 {
        vec![alg::ECDSA_P384_SHA384, alg::ECDSA_P256_SHA384]
    } else if *sig == oid::OID_SIG_ED25519 {
        vec![alg::ED25519]
    } else {
        Vec::new()
    }
}

/// TLS 1.3 забороняє PKCS#1 v1.5 і SHA-1 у підписі рукостискання (RFC 8446, 4.2.3).
fn allowed_in_tls13(scheme: SignatureScheme) -> bool {
    !matches!(
        scheme,
        SignatureScheme::RSA_PKCS1_SHA1
            | SignatureScheme::RSA_PKCS1_SHA256
            | SignatureScheme::RSA_PKCS1_SHA384
            | SignatureScheme::RSA_PKCS1_SHA512
            | SignatureScheme::ECDSA_SHA1_Legacy
    )
}

/// Чи підписав `issuer_key` сертифікат `cert`.
fn signed_by(cert: &X509Certificate<'_>, issuer_key: &[u8]) -> Result<bool, TlsProblem> {
    let algs = cert_algorithms(&cert.signature_algorithm.algorithm);
    if algs.is_empty() {
        return Err(TlsProblem::UnsupportedSignature);
    }
    let tbs = cert.tbs_certificate.as_ref();
    let signature = &cert.signature_value.data;
    Ok(algs
        .iter()
        .any(|a| a.verify_signature(issuer_key, tbs, signature).is_ok()))
}

fn check_time(cert: &X509Certificate<'_>, now: i64) -> Result<(), TlsProblem> {
    let validity = cert.validity();
    if now < validity.not_before.timestamp() {
        return Err(TlsProblem::NotYetValid);
    }
    if now > validity.not_after.timestamp() {
        return Err(TlsProblem::Expired);
    }
    Ok(())
}

fn is_ca(cert: &X509Certificate<'_>) -> bool {
    matches!(cert.basic_constraints(), Ok(Some(bc)) if bc.value.ca)
}

/// Серійний номер із CN листового сертифіката (для діагностики й майстра).
pub fn common_name(der: &[u8]) -> Option<String> {
    let (_, cert) = X509Certificate::from_der(der).ok()?;
    cert.subject()
        .iter_common_name()
        .next()?
        .as_str()
        .ok()
        .map(str::to_owned)
}

fn verify_chain(
    serial: &str,
    anchors: &[Anchor],
    end_entity: &[u8],
    intermediates: &[CertificateDer<'_>],
    now: i64,
) -> Result<Vec<usize>, TlsProblem> {
    let (_, leaf) = X509Certificate::from_der(end_entity).map_err(|_| TlsProblem::BadEncoding)?;
    check_time(&leaf, now)?;
    let parsed: Vec<X509Certificate<'_>> = intermediates
        .iter()
        .map(|der| {
            X509Certificate::from_der(der)
                .map(|(_, c)| c)
                .map_err(|_| TlsProblem::BadEncoding)
        })
        .collect::<Result<_, _>>()?;
    let mut current = &leaf;
    // Індекси проміжних, через які пройшов ланцюжок (для кешу).
    let mut used = Vec::new();
    for _ in 0..MAX_DEPTH {
        for anchor in anchors {
            if signed_by(current, &anchor.key)? {
                // Ланцюжок справжній; лише тепер ім’я має значення.
                let cn = leaf
                    .subject()
                    .iter_common_name()
                    .next()
                    .and_then(|cn| cn.as_str().ok());
                return match cn {
                    Some(cn) if cn.eq_ignore_ascii_case(serial) => Ok(used),
                    _ => Err(TlsProblem::SerialMismatch),
                };
            }
        }
        let mut next = None;
        for (index, candidate) in parsed.iter().enumerate() {
            if std::ptr::eq(candidate, current)
                || used.contains(&index)
                || !signed_by(current, &candidate.public_key().subject_public_key.data)?
            {
                continue;
            }
            if !is_ca(candidate) {
                return Err(TlsProblem::NotCa);
            }
            check_time(candidate, now)?;
            used.push(index);
            next = Some(candidate);
            break;
        }
        match next {
            Some(n) => current = n,
            None => return Err(TlsProblem::UnknownIssuer),
        }
    }
    Err(TlsProblem::UnknownIssuer)
}

/// Підпис рукостискання ключем із листового сертифіката (працює і для v1).
fn verify_handshake(
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
    schemes: &WebPkiSupportedAlgorithms,
    tls13: bool,
) -> Result<HandshakeSignatureValid, TlsError> {
    let (_, leaf) = X509Certificate::from_der(cert)
        .map_err(|_| TlsError::InvalidCertificate(CertificateError::BadEncoding))?;
    let key = &leaf.public_key().subject_public_key.data;
    let candidates = schemes
        .mapping
        .iter()
        .find(|(scheme, _)| *scheme == dss.scheme)
        .map(|(_, algs)| *algs)
        .ok_or(TlsError::PeerMisbehaved(
            rustls::PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme,
        ))?;
    let candidates = if tls13 {
        &candidates[..candidates.len().min(1)]
    } else {
        candidates
    };
    if candidates
        .iter()
        .any(|a| a.verify_signature(key, message, dss.signature()).is_ok())
    {
        Ok(HandshakeSignatureValid::assertion())
    } else {
        Err(TlsError::InvalidCertificate(CertificateError::BadSignature))
    }
}

/// Верифікатор одного принтера.
pub struct PrinterVerifier {
    serial: String,
    anchors: Arc<Vec<Anchor>>,
    cache: IntermediateCache,
    schemes: WebPkiSupportedAlgorithms,
    problem: ProblemSlot,
}

impl fmt::Debug for PrinterVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrinterVerifier").finish_non_exhaustive()
    }
}

impl ServerCertVerifier for PrinterVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let now = i64::try_from(now.as_secs()).unwrap_or(i64::MAX);
        // Надіслані сервером проміжні + ті, що вже довели свій ланцюжок до кореня Bambu
        // раніше: камера P2S (порт 6000) не надсилає «BBL Device CA N7-V2», а MQTT — надсилає.
        let mut chain: Vec<CertificateDer<'_>> = intermediates.to_vec();
        chain.extend(self.cache.snapshot());
        match verify_chain(&self.serial, &self.anchors, end_entity, &chain, now) {
            Ok(used) => {
                self.cache.remember(used.iter().map(|&i| chain[i].clone().into_owned()));
                Ok(ServerCertVerified::assertion())
            }
            Err(problem) => {
                let error = match problem {
                    TlsProblem::BadEncoding => CertificateError::BadEncoding,
                    TlsProblem::NotYetValid => CertificateError::NotValidYet,
                    TlsProblem::Expired => CertificateError::Expired,
                    TlsProblem::UnknownIssuer | TlsProblem::NotCa => CertificateError::UnknownIssuer,
                    TlsProblem::UnsupportedSignature => CertificateError::BadSignature,
                    TlsProblem::SerialMismatch => CertificateError::NotValidForName,
                };
                self.problem.set(problem);
                Err(TlsError::InvalidCertificate(error))
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_handshake(message, cert, dss, &self.schemes, false)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        if !allowed_in_tls13(dss.scheme) {
            return Err(TlsError::PeerMisbehaved(
                rustls::PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme,
            ));
        }
        verify_handshake(message, cert, dss, &self.schemes, true)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes.supported_schemes()
    }
}

/// Конфігурація TLS-клієнта для принтера `serial`. Один `Arc<ClientConfig>` на принтер
/// спільний для керувального й даних з’єднань FTPS: так rustls відновлює TLS-сесію.
pub fn client_config(serial: &str) -> (Arc<ClientConfig>, ProblemSlot) {
    config_with(serial, bundled(), shared_cache())
}

/// Те саме з власними коренями (PEM) — для тестів на синтетичних сертифікатах.
pub fn client_config_with_roots(serial: &str, roots_pem: &[u8]) -> (Arc<ClientConfig>, ProblemSlot) {
    config_with(
        serial,
        Arc::new(anchors_from(&parse_pem(roots_pem))),
        IntermediateCache::default(),
    )
}

fn make_verifier(
    serial: &str,
    anchors: Arc<Vec<Anchor>>,
    cache: IntermediateCache,
) -> (PrinterVerifier, ProblemSlot) {
    let problem = ProblemSlot::default();
    let verifier = PrinterVerifier {
        serial: serial.to_owned(),
        anchors,
        cache,
        schemes: provider().signature_verification_algorithms,
        problem: problem.clone(),
    };
    (verifier, problem)
}

fn config_with(
    serial: &str,
    anchors: Arc<Vec<Anchor>>,
    cache: IntermediateCache,
) -> (Arc<ClientConfig>, ProblemSlot) {
    let (verifier, problem) = make_verifier(serial, anchors, cache);
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("ring підтримує TLS 1.2 і 1.3")
        // `dangerous` — лише назва API для власного верифікатора; перевірка повна (див. вище).
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    (Arc::new(config), problem)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CA: &[u8] = include_bytes!("../tests/fixtures/ca.pem");
    const ROGUE: &[u8] = include_bytes!("../tests/fixtures/rogue.pem");
    const LEAF: &[u8] = include_bytes!("../tests/fixtures/leaf.pem");
    const ROGUE_LEAF: &[u8] = include_bytes!("../tests/fixtures/rogue_leaf.pem");
    const INT: &[u8] = include_bytes!("../tests/fixtures/int.pem");
    const INT_LEAF: &[u8] = include_bytes!("../tests/fixtures/int_leaf.pem");
    /// 2030-01-01: усередині терміну дії фікстур.
    const NOW: i64 = 1_893_456_000;

    fn one(pem: &[u8]) -> CertificateDer<'static> {
        parse_pem(pem).remove(0)
    }
    fn check(serial: &str, roots: &[u8], leaf: &[u8], chain: &[&[u8]], now: i64) -> Result<(), TlsProblem> {
        let anchors = anchors_from(&parse_pem(roots));
        let chain: Vec<_> = chain.iter().map(|p| one(p)).collect();
        verify_chain(serial, &anchors, &one(leaf), &chain, now).map(|_| ())
    }

    #[test]
    fn bundled_bambu_roots_parse() {
        assert_eq!(parse_pem(BBL_CA_PEM).len(), 5);
        assert_eq!(bundled().len(), 5);
    }

    #[test]
    fn v1_leaf_signed_by_trusted_ca_with_matching_serial_passes() {
        assert_eq!(check("TESTSERIAL0001", CA, LEAF, &[], NOW), Ok(()));
        // Серійні номери на екрані й у SSDP — верхній регістр; регістр не важить.
        assert_eq!(check("testserial0001", CA, LEAF, &[], NOW), Ok(()));
        assert_eq!(common_name(&one(LEAF)).as_deref(), Some("TESTSERIAL0001"));
    }

    #[test]
    fn wrong_serial_untrusted_ca_and_time_are_rejected() {
        assert_eq!(
            check("TESTSERIAL0009", CA, LEAF, &[], NOW),
            Err(TlsProblem::SerialMismatch)
        );
        assert_eq!(
            check("TESTSERIAL0001", CA, ROGUE_LEAF, &[], NOW),
            Err(TlsProblem::UnknownIssuer)
        );
        // Чужий CA, переданий сервером як «проміжний», нічого не дає.
        assert_eq!(
            check("TESTSERIAL0001", CA, ROGUE_LEAF, &[ROGUE], NOW),
            Err(TlsProblem::UnknownIssuer)
        );
        // Справжній лист не довіряється чужому кореню.
        assert_eq!(
            check("TESTSERIAL0001", ROGUE, LEAF, &[], NOW),
            Err(TlsProblem::UnknownIssuer)
        );
        assert_eq!(
            check("TESTSERIAL0001", CA, LEAF, &[], 0),
            Err(TlsProblem::NotYetValid)
        );
        assert_eq!(
            check("TESTSERIAL0001", CA, LEAF, &[], i64::MAX / 2),
            Err(TlsProblem::Expired)
        );
        // Бандл Bambu не довіряє тестовому CA.
        assert_eq!(
            check("TESTSERIAL0001", BBL_CA_PEM, LEAF, &[], NOW),
            Err(TlsProblem::UnknownIssuer)
        );
    }

    #[test]
    fn intermediate_ecdsa_chain_passes_only_with_intermediate() {
        assert_eq!(check("TESTSERIAL0002", CA, INT_LEAF, &[INT], NOW), Ok(()));
        assert_eq!(
            check("TESTSERIAL0002", CA, INT_LEAF, &[], NOW),
            Err(TlsProblem::UnknownIssuer)
        );
        // Лист, а не CA, у ролі проміжного — відмова.
        assert_eq!(
            check("TESTSERIAL0002", CA, INT_LEAF, &[LEAF], NOW),
            Err(TlsProblem::UnknownIssuer)
        );
    }

    #[test]
    fn verified_intermediate_is_cached_for_connections_without_it() {
        // Як P2S: MQTT надсилає проміжний CA, камера — лише лист.
        let anchors = Arc::new(anchors_from(&parse_pem(CA)));
        let (verifier, _) =
            make_verifier("TESTSERIAL0002", anchors.clone(), IntermediateCache::default());
        let leaf = one(INT_LEAF);
        let name = ServerName::try_from("10.0.0.1").unwrap();
        let now = UnixTime::since_unix_epoch(std::time::Duration::from_secs(NOW as u64));
        assert!(verifier.verify_server_cert(&leaf, &[], &name, &[], now).is_err());
        assert!(verifier.verify_server_cert(&leaf, &[one(INT)], &name, &[], now).is_ok());
        assert!(verifier.verify_server_cert(&leaf, &[], &name, &[], now).is_ok());
        // Кеш не робить довіреним чужий ланцюжок.
        let rogue = one(ROGUE_LEAF);
        let (verifier, _) = make_verifier("TESTSERIAL0001", anchors, IntermediateCache::default());
        assert!(verifier.verify_server_cert(&rogue, &[one(ROGUE)], &name, &[], now).is_err());
        assert!(verifier.verify_server_cert(&rogue, &[], &name, &[], now).is_err());
    }

    #[test]
    fn garbage_is_bad_encoding() {
        let anchors = anchors_from(&parse_pem(CA));
        assert_eq!(
            verify_chain("X", &anchors, b"not a certificate", &[], NOW).map(|_| ()),
            Err(TlsProblem::BadEncoding)
        );
    }
}
