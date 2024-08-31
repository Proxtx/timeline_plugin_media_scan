use rsa::pkcs1v15::{SigningKey, VerifyingKey};
use rsa::sha2::{Digest, Sha256};
use rsa::signature::{Keypair, RandomizedSigner, SignatureEncoding, Verifier};
use rsa::RsaPrivateKey;

fn main() {
    let mut rng = rand::thread_rng();

    let bits = 2048;
    let private_key = RsaPrivateKey::new(&mut rng, bits).expect("failed to generate a key");
    println!(
        "Private Key (between arrows): -->{}<--",
        toml::to_string(&private_key).unwrap()
    );
}
