#![feature(async_await, await_macro, futures_api)]

use std::env;
use std::io::{stdin, stdout, Write};
use futures::executor::block_on;
use futures::io::AllowStdIo;
use shs_async::*;

extern crate readwrite;
use readwrite::ReadWrite;

extern crate hex;
use hex::FromHex;

// For use with https://github.com/AljoschaMeyer/shs1-testsuite
fn main() -> Result<(), HandshakeError> {

    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        println!("Usage: test_client net_id_as_hex server_pk_as_hex");
        std::process::exit(1);
    }

    let net_id = NetworkId::from_slice(&Vec::from_hex(&args[1]).unwrap()).unwrap();
    let server_pk = ServerPublicKey::from_slice(&Vec::from_hex(&args[2]).unwrap()).unwrap();

    let (pk, sk) = client::generate_longterm_keypair();

    let mut stream = AllowStdIo::new(ReadWrite::new(stdin(), stdout()));
    let mut o = block_on(client(&mut stream, net_id, pk, sk, server_pk))?;

    let mut v = o.c2s_key.as_slice().to_vec();
    v.extend_from_slice(o.c2s_noncegen.next().as_slice());
    v.extend_from_slice(o.s2c_key.as_slice());
    v.extend_from_slice(o.s2c_noncegen.next().as_slice());
    assert_eq!(v.len(), 112);

    stdout().write(&v).unwrap();
    stdout().flush().unwrap();

    Ok(())
}
