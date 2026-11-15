//! Authenticated checkpoint envelopes. Not transparent block-device encryption.
//! Keys are 256 random bits, not passwords; the caller owns their lifecycle.
use super::{Error, Loaded, Volume};
use crate::filesystem::Credentials;
use ring::{
    aead,
    rand::{SecureRandom, SystemRandom},
};
use zeroize::Zeroizing;
const MAGIC: &[u8; 8] = b"ELARASE1";
const PREFIX: usize = 20;
const MAX: usize = 1024 * 1024;
pub fn new_key() -> Result<Zeroizing<[u8; 32]>, Error> {
    let mut key = Zeroizing::new([0; 32]);
    SystemRandom::new()
        .fill(key.as_mut())
        .map_err(|_| Error::Crypto)?;
    Ok(key)
}
fn key(bytes: &[u8]) -> Result<aead::LessSafeKey, Error> {
    Ok(aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, bytes).map_err(|_| Error::Crypto)?,
    ))
}
impl Volume {
    pub fn sealed_image(
        &self,
        actor: &Credentials,
        secret: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        let cipher = key(secret)?;
        let (image, _) = self.image(actor)?;
        let mut payload = Zeroizing::new(image);
        if payload.len() > MAX - PREFIX - aead::CHACHA20_POLY1305.tag_len() {
            return Err(Error::Quota);
        }
        let mut nonce = [0; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| Error::Crypto)?;
        cipher
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(MAGIC.as_slice()),
                &mut *payload,
            )
            .map_err(|_| Error::Crypto)?;
        let mut output = Zeroizing::new(Vec::with_capacity(PREFIX + payload.len()));
        output.extend_from_slice(MAGIC);
        output.extend_from_slice(&nonce);
        output.extend_from_slice(&payload);
        Ok(output)
    }
    pub fn load_sealed_image(bytes: &[u8], secret: &[u8]) -> Result<Loaded, Error> {
        if bytes.len() > MAX
            || bytes.len() < PREFIX + aead::CHACHA20_POLY1305.tag_len()
            || &bytes[..8] != MAGIC
        {
            return Err(Error::Crypto);
        }
        let cipher = key(secret)?;
        let nonce: [u8; 12] = bytes[8..PREFIX].try_into().map_err(|_| Error::Crypto)?;
        let mut plaintext = Zeroizing::new(bytes[PREFIX..].to_vec());
        let decoded = cipher
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(MAGIC.as_slice()),
                &mut plaintext,
            )
            .map_err(|_| Error::Crypto)?;
        Self::load_image(decoded)
    }
}
