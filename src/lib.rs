
#![feature(async_await, await_macro, futures_api)]

extern crate futures;
extern crate shs_core;

use futures::io::{
    AsyncRead,
    AsyncReadExt,
    AsyncWrite,
    AsyncWriteExt,
};
use core::mem::size_of;

use ssb_crypto::{NetworkKey, NonceGen, PublicKey, SecretKey};
use shs_core::{*, messages::*};

pub use shs_core::HandshakeError;

pub async fn client<S: AsyncRead + AsyncWrite>(mut stream: S,
                                               net_key: NetworkKey,
                                               pk: PublicKey,
                                               sk: SecretKey,
                                               server_pk: PublicKey)
                                               -> Result<HandshakeOutcome, HandshakeError> {
    let r = await!(attempt_client_side(&mut stream, net_key, pk, sk, server_pk));
    if r.is_err() {
        await!(stream.close()).unwrap_or(());
    }
    r
}

async fn attempt_client_side<S: AsyncRead + AsyncWrite>(mut stream: S,
                                                        net_key: NetworkKey,
                                                        pk: PublicKey,
                                                        sk: SecretKey,
                                                        server_pk: PublicKey)
                                                        -> Result<HandshakeOutcome, HandshakeError> {

    let pk = ClientPublicKey(pk);
    let sk = ClientSecretKey(sk);
    let server_pk = ServerPublicKey(server_pk);

    let (eph_pk, eph_sk) = client::generate_eph_keypair();
    let hello = ClientHello::new(&eph_pk, &net_key);
    await!(stream.write_all(&hello.as_slice()))?;
    await!(stream.flush())?;

    let server_eph_pk = {
        let mut buf = [0u8; size_of::<ServerHello>()];
        await!(stream.read_exact(&mut buf))?;

        let server_hello = ServerHello::from_slice(&buf)?;
        server_hello.verify(&net_key)?
    };

    // Derive shared secrets
    let shared_a = SharedA::client_side(&eph_sk, &server_eph_pk)?;
    let shared_b = SharedB::client_side(&eph_sk, &server_pk)?;
    let shared_c = SharedC::client_side(&sk, &server_eph_pk)?;

    // Send client auth
    let client_auth = ClientAuth::new(&sk, &pk, &server_pk, &net_key, &shared_a, &shared_b);
    await!(stream.write_all(client_auth.as_slice()))?;
    await!(stream.flush())?;

    let mut buf = [0u8; 80];
    await!(stream.read_exact(&mut buf))?;

    let server_acc = ServerAccept::from_buffer(buf.to_vec())?;
    server_acc.open_and_verify(&sk, &pk, &server_pk,
                               &net_key, &shared_a,
                               &shared_b, &shared_c)?;

    Ok(HandshakeOutcome {
        read_key: server_to_client_key(&pk, &net_key, &shared_a, &shared_b, &shared_c),
        read_noncegen: NonceGen::new(&eph_pk.0, &net_key),

        write_key: client_to_server_key(&server_pk, &net_key, &shared_a, &shared_b, &shared_c),
        write_noncegen: NonceGen::new(&server_eph_pk.0, &net_key),
    })
}

pub async fn server<S: AsyncRead + AsyncWrite>(mut stream: S,
                                               net_key: NetworkKey,
                                               pk: PublicKey,
                                               sk: SecretKey)
                                               -> Result<HandshakeOutcome, HandshakeError> {
    let r = await!(attempt_server_side(&mut stream, net_key, pk, sk));
    if r.is_err() {
        await!(stream.close()).unwrap_or(());
    }
    r
}

async fn attempt_server_side<S: AsyncRead + AsyncWrite>(mut stream: S,
                                                        net_key: NetworkKey,
                                                        pk: PublicKey,
                                                        sk: SecretKey)
                                                        -> Result<HandshakeOutcome, HandshakeError> {

    let pk = ServerPublicKey(pk);
    let sk = ServerSecretKey(sk);

    let (eph_pk, eph_sk) = server::generate_eph_keypair();

    // Receive and verify client hello
    let client_eph_pk = {
        let mut buf = [0u8; 64];
        await!(stream.read_exact(&mut buf))?;
        let client_hello = ClientHello::from_slice(&buf)?;
        client_hello.verify(&net_key)?
    };

    // Send server hello
    let hello = ServerHello::new(&eph_pk, &net_key);
    await!(stream.write_all(hello.as_slice()))?;
    await!(stream.flush())?;

    // Derive shared secrets
    let shared_a = SharedA::server_side(&eph_sk, &client_eph_pk)?;
    let shared_b = SharedB::server_side(&sk, &client_eph_pk)?;

    // Receive and verify client auth
    let (client_sig, client_pk) = {
        let mut buf = [0u8; 112];
        await!(stream.read_exact(&mut buf))?;

        let client_auth = ClientAuth::from_buffer(buf.to_vec())?;
        client_auth.open_and_verify(&pk, &net_key, &shared_a, &shared_b)?
    };

    // Derive shared secret
    let shared_c = SharedC::server_side(&eph_sk, &client_pk)?;

    // Send server accept
    let server_acc = ServerAccept::new(&sk, &client_pk, &net_key, &client_sig,
                                       &shared_a, &shared_b, &shared_c);
    await!(stream.write_all(server_acc.as_slice()))?;
    await!(stream.flush())?;

    Ok(HandshakeOutcome {
        read_key: client_to_server_key(&pk, &net_key, &shared_a, &shared_b, &shared_c),
        read_noncegen: NonceGen::new(&eph_pk.0, &net_key),

        write_key: server_to_client_key(&client_pk, &net_key, &shared_a, &shared_b, &shared_c),
        write_noncegen: NonceGen::new(&client_eph_pk.0, &net_key),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::task::Waker;
    use std::io;
    use futures::{join, Poll};
    use futures::executor::block_on;

    extern crate async_ringbuffer;
    use ssb_crypto::{generate_longterm_keypair, NetworkKey, PublicKey};

    struct Duplex<R, W> {
        r: R,
        w: W,
    }
    impl<R: AsyncRead, W> AsyncRead for Duplex<R, W> {
        fn poll_read(&mut self, wk: &Waker, buf: &mut [u8]) -> Poll<Result<usize, io::Error>> {
            self.r.poll_read(wk, buf)
        }
    }
    impl<R, W: AsyncWrite> AsyncWrite for Duplex<R, W> {
        fn poll_write(&mut self, wk: &Waker, buf: &[u8]) -> Poll<Result<usize, io::Error>> {
            self.w.poll_write(wk, buf)
        }
        fn poll_flush(&mut self, wk: &Waker) -> Poll<Result<(), io::Error>> {
            self.w.poll_flush(wk)
        }
        fn poll_close(&mut self, wk: &Waker) -> Poll<Result<(), io::Error>> {
            self.w.poll_close(wk)
        }
    }

    #[test]
    fn basic() {
        let (c2s_w, c2s_r) = async_ringbuffer::ring_buffer(1024);
        let (s2c_w, s2c_r) = async_ringbuffer::ring_buffer(1024);
        let mut c_stream = Duplex { r: s2c_r, w: c2s_w };
        let mut s_stream = Duplex { r: c2s_r, w: s2c_w };

        let (s_pk, s_sk) = generate_longterm_keypair();
        let (c_pk, c_sk) = generate_longterm_keypair();

        let net_key = NetworkKey::SSB_MAIN_NET;
        let client_side = client(&mut c_stream, net_key.clone(), c_pk, c_sk, s_pk.clone());
        let server_side = server(&mut s_stream, net_key.clone(), s_pk, s_sk);

        let (c_out, s_out) = block_on(async {
            join!(client_side, server_side)
        });

        let mut c_out = c_out.unwrap();
        let mut s_out = s_out.unwrap();

        assert_eq!(c_out.write_key, s_out.read_key);
        assert_eq!(c_out.read_key, s_out.write_key);

        assert_eq!(c_out.write_noncegen.next(),
                   s_out.read_noncegen.next());

        assert_eq!(c_out.read_noncegen.next(),
                   s_out.write_noncegen.next());
    }

    #[test]
    fn reject_wrong_server_pk() {
        test_handshake_with_bad_server_pk(
            PublicKey::from_slice(&[ 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
		                     0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
		                     0, 0, 0, 0, 0, 0, 0, 0, 0, 0 ]).unwrap());

        let (pk, _sk) = generate_longterm_keypair();
        test_handshake_with_bad_server_pk(pk);
    }

    fn test_handshake_with_bad_server_pk(bad_pk: PublicKey) {
        let (c2s_w, c2s_r) = async_ringbuffer::ring_buffer(1024);
        let (s2c_w, s2c_r) = async_ringbuffer::ring_buffer(1024);
        let mut c_stream = Duplex { r: s2c_r, w: c2s_w };
        let mut s_stream = Duplex { r: c2s_r, w: s2c_w };

        let (s_pk, s_sk) = generate_longterm_keypair();
        let (c_pk, c_sk) = generate_longterm_keypair();

        let net_key = NetworkKey::SSB_MAIN_NET;

        let client_side = client(&mut c_stream, net_key.clone(), c_pk, c_sk, bad_pk);
        let server_side = server(&mut s_stream, net_key.clone(), s_pk, s_sk);

        let (c_out, s_out) = block_on(async {
            join!(client_side, server_side)
        });

        assert!(c_out.is_err());
        assert!(s_out.is_err());

        // let mut c_out = c_out.unwrap();
        // let mut s_out = s_out.unwrap();




    }


}
