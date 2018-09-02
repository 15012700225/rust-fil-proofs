#[macro_use]
extern crate criterion;
extern crate bellman;
#[macro_use]
extern crate bitvec;
extern crate pairing;
extern crate proofs;
extern crate rand;
extern crate sapling_crypto;

use bellman::groth16::*;
use bellman::{Circuit, ConstraintSystem, SynthesisError};
use circuit::bench::BenchCS;
use criterion::{black_box, Criterion, ParameterizedBenchmark};
use pairing::bls12_381::Bls12;
use proofs::circuit;
use proofs::crypto::pedersen;
use rand::{thread_rng, Rng};
use sapling_crypto::circuit::boolean::{self, Boolean};
use sapling_crypto::jubjub::{JubjubBls12, JubjubEngine};

struct PedersenExample<'a, E: JubjubEngine> {
    params: &'a E::Params,
    data: &'a [Option<bool>],
}

impl<'a, E: JubjubEngine> Circuit<E> for PedersenExample<'a, E> {
    fn synthesize<CS: ConstraintSystem<E>>(self, cs: &mut CS) -> Result<(), SynthesisError> {
        let data: Vec<Boolean> = self
            .data
            .into_iter()
            .enumerate()
            .map(|(i, b)| {
                Ok(Boolean::from(boolean::AllocatedBit::alloc(
                    cs.namespace(|| format!("bit {}", i)),
                    *b,
                )?))
            }).collect::<Result<Vec<_>, SynthesisError>>()?;

        let cs = cs.namespace(|| "pedersen");
        let res = circuit::pedersen::pedersen_compression_num(cs, self.params, &data)?;
        // please compiler don't optimize the result away
        // only check if we actually have input data
        if self.data[0].is_some() {
            res.get_value().unwrap();
        }

        Ok(())
    }
}

fn pedersen_benchmark(c: &mut Criterion) {
    // FIXME: We're duplicating these params because of compiler errors, presumably related to
    // the move closures. There must be a better way.
    let jubjub_params = JubjubBls12::new();
    let jubjub_params2 = JubjubBls12::new();
    let mut rng1 = thread_rng();
    let rng2 = thread_rng();
    let groth_params = generate_random_parameters::<Bls12, _, _>(
        PedersenExample {
            params: &jubjub_params,
            data: &vec![None; 512],
        },
        &mut rng1,
    ).unwrap();

    let params = vec![64];

    c.bench(
        "pedersen",
        ParameterizedBenchmark::new(
            "non-circuit bytes",
            |b, bytes| {
                let mut rng = thread_rng();
                let data: Vec<u8> = (0..*bytes).map(|_| rng.gen()).collect();
                let mut out = bitvec![LittleEndian, u8; 0; *bytes * 8];

                b.iter(|| black_box(pedersen::pedersen_compression(&mut out, bytes * 8)))
            },
            params,
        ).with_function("non-circuit bits", |b, bytes| {
            let mut rng = thread_rng();
            let mut data: Vec<bool> = (0..bytes * 8).map(|_| rng.gen()).collect();
            let l = data.len();
            b.iter(|| black_box(pedersen::pedersen_compression_z(&mut data, l)));
        }).with_function("circuit - create proof", move |b, bytes| {
            b.iter(|| {
                let mut rng = rng1.clone();
                let data: Vec<Option<bool>> = (0..bytes * 8).map(|_| Some(rng.gen())).collect();

                let proof = create_random_proof(
                    PedersenExample {
                        params: &jubjub_params,
                        data: data.as_slice(),
                    },
                    &groth_params,
                    &mut rng,
                ).unwrap();

                black_box(proof)
            });
        }).with_function("circuit - just synthesize circuit", move |b, bytes| {
            b.iter(|| {
                let mut cs = BenchCS::<Bls12>::new();

                let mut rng = rng2.clone();
                let data: Vec<Option<bool>> = (0..bytes * 8).map(|_| Some(rng.gen())).collect();

                PedersenExample {
                    params: &jubjub_params2,
                    data: data.as_slice(),
                }.synthesize(&mut cs)
                .unwrap();

                black_box(cs)
            });
        }).sample_size(20),
    );
}

criterion_group!(benches, pedersen_benchmark);
criterion_main!(benches);
