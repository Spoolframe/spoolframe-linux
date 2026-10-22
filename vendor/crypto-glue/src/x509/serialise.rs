pub mod pkeyb64 {
    use crate::der::SecretDocument;
    use base64::{engine::general_purpose, Engine as _};
    use serde::{de::Error as DeError, Deserialize, Deserializer, Serializer};
    use tracing::error;

    pub fn serialize<S>(key: &SecretDocument, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let s = general_purpose::URL_SAFE_NO_PAD.encode(key.as_bytes());

        ser.serialize_str(&s)
    }

    pub fn deserialize<'de, D>(des: D) -> Result<SecretDocument, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = <&str>::deserialize(des)?;
        let s = general_purpose::URL_SAFE_NO_PAD
            .decode(raw)
            .or_else(|_| general_purpose::URL_SAFE.decode(raw))
            .map_err(|err| {
                error!(?err, "base64 url-safe invalid");
                D::Error::custom("base64 url-safe invalid")
            })?;

        SecretDocument::try_from(s.as_slice()).map_err(|err| {
            error!(?err, "pkey invalid der");
            D::Error::custom("pkey invalid der")
        })
    }
}

pub mod x509b64 {
    use crate::{
        traits::{DecodeDer, EncodeDer},
        x509::Certificate,
    };
    use base64::{engine::general_purpose, Engine as _};
    use serde::{
        de::Error as DeError, ser::Error as SerError, Deserialize, Deserializer, Serializer,
    };
    use tracing::error;

    pub fn cert_to_string(cert: &Certificate) -> Result<String, crate::der::Error> {
        cert.to_der()
            .inspect_err(|err| {
                error!(?err, "cert to_der");
            })
            .map(|der| general_purpose::URL_SAFE_NO_PAD.encode(der))
    }

    pub fn serialize<S>(cert: &Certificate, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let der = cert.to_der().map_err(|err| {
            error!(?err, "cert to_der");
            S::Error::custom("cert to_der")
        })?;
        let s = general_purpose::URL_SAFE_NO_PAD.encode(der);

        ser.serialize_str(&s)
    }

    pub fn deserialize<'de, D>(des: D) -> Result<Certificate, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = <String>::deserialize(des)?;
        let s = general_purpose::URL_SAFE_NO_PAD
            .decode(&raw)
            .or_else(|_| general_purpose::URL_SAFE.decode(&raw))
            .map_err(|err| {
                error!(?err, "base64 url-safe invalid");
                D::Error::custom("base64 url-safe invalid")
            })?;

        Certificate::from_der(&s).map_err(|err| {
            error!(?err, "x509 invalid der");
            D::Error::custom("x509 invalid der")
        })
    }
}
