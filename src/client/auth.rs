use super::security;
use crate::{VncError, VncLimits, VncVersion};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum SecurityType {
    Invalid = 0,
    None = 1,
    VncAuth = 2,
    RA2 = 5,
    RA2ne = 6,
    Tight = 16,
    Ultra = 17,
    Tls = 18,
    VeNCrypt = 19,
    GtkVncSasl = 20,
    Md5Hash = 21,
    ColinDeanXvp = 22,
}

impl TryFrom<u8> for SecurityType {
    type Error = VncError;

    fn try_from(num: u8) -> Result<Self, Self::Error> {
        match num {
            0 => Ok(Self::Invalid),
            1 => Ok(Self::None),
            2 => Ok(Self::VncAuth),
            5 => Ok(Self::RA2),
            6 => Ok(Self::RA2ne),
            16 => Ok(Self::Tight),
            17 => Ok(Self::Ultra),
            18 => Ok(Self::Tls),
            19 => Ok(Self::VeNCrypt),
            20 => Ok(Self::GtkVncSasl),
            21 => Ok(Self::Md5Hash),
            22 => Ok(Self::ColinDeanXvp),
            invalid => Err(VncError::InvalidSecurityType(u32::from(invalid))),
        }
    }
}

impl From<SecurityType> for u8 {
    fn from(e: SecurityType) -> Self {
        e as u8
    }
}

async fn read_bounded_string<S>(
    reader: &mut S,
    field: &'static str,
    limit: usize,
) -> Result<String, VncError>
where
    S: AsyncRead + Unpin,
{
    let len = reader.read_u32().await? as usize;
    if len > limit {
        return Err(VncError::LimitExceeded {
            field,
            actual: len as u64,
            limit: limit as u64,
        });
    }
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

impl SecurityType {
    pub(super) async fn read<S>(
        reader: &mut S,
        version: &VncVersion,
        limits: &VncLimits,
    ) -> Result<Vec<Self>, VncError>
    where
        S: AsyncRead + Unpin,
    {
        match version {
            VncVersion::RFB33 => {
                let raw = reader.read_u32().await?;
                let security_type = u8::try_from(raw)
                    .map_err(|_| VncError::InvalidSecurityType(raw))?
                    .try_into()?;
                if security_type == SecurityType::Invalid {
                    let reason = read_bounded_string(
                        reader,
                        "security failure reason",
                        limits.max_failure_reason_bytes,
                    )
                    .await?;
                    return Err(VncError::SecurityFailure(reason));
                }
                Ok(vec![security_type])
            }
            VncVersion::RFB37 | VncVersion::RFB38 => {
                let num = reader.read_u8().await?;
                if num == 0 {
                    let reason = read_bounded_string(
                        reader,
                        "security failure reason",
                        limits.max_failure_reason_bytes,
                    )
                    .await?;
                    return Err(VncError::SecurityFailure(reason));
                }
                let mut sec_types = Vec::with_capacity(num as usize);
                for _ in 0..num {
                    sec_types.push(reader.read_u8().await?.try_into()?);
                }
                tracing::trace!("Server supported security type: {:?}", sec_types);
                Ok(sec_types)
            }
        }
    }

    pub(super) async fn write<S>(&self, writer: &mut S) -> Result<(), VncError>
    where
        S: AsyncWrite + Unpin,
    {
        writer.write_all(&[(*self).into()]).await?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AuthResult {
    Ok,
    Failed,
}

impl TryFrom<u32> for AuthResult {
    type Error = VncError;

    fn try_from(num: u32) -> Result<Self, Self::Error> {
        match num {
            0 => Ok(Self::Ok),
            1 => Ok(Self::Failed),
            invalid => Err(VncError::InvalidSecurityResult(invalid)),
        }
    }
}

pub(super) struct AuthHelper {
    challenge: [u8; 16],
    key: [u8; 8],
}

impl AuthHelper {
    pub(super) async fn read<S>(reader: &mut S, credential: &str) -> Result<Self, VncError>
    where
        S: AsyncRead + Unpin,
    {
        let mut challenge = [0; 16];
        reader.read_exact(&mut challenge).await?;

        let mut key = [0u8; 8];
        for (i, key_i) in key.iter_mut().enumerate() {
            let c = credential.as_bytes().get(i).copied().unwrap_or(0);
            let mut cs = 0u8;
            for j in 0..8 {
                cs |= ((c >> j) & 1) << (7 - j);
            }
            *key_i = cs;
        }

        Ok(Self { challenge, key })
    }

    pub(super) async fn write<S>(&self, writer: &mut S) -> Result<(), VncError>
    where
        S: AsyncWrite + Unpin,
    {
        let encrypted = security::des::encrypt(&self.challenge, &self.key);
        writer.write_all(&encrypted).await?;
        Ok(())
    }

    pub(super) async fn finish<S>(self, reader: &mut S) -> Result<AuthResult, VncError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        reader.read_u32().await?.try_into()
    }
}

pub(super) async fn read_security_failure<S>(
    reader: &mut S,
    limits: &VncLimits,
) -> Result<String, VncError>
where
    S: AsyncRead + Unpin,
{
    read_bounded_string(
        reader,
        "security failure reason",
        limits.max_failure_reason_bytes,
    )
    .await
}

