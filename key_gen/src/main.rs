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
