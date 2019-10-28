use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use storage_proofs::drgraph::DefaultTreeHasher;
use storage_proofs::hasher::Hasher;
use storage_proofs::pieces::generate_piece_commitment_bytes_from_source;
use storage_proofs::porep::PoRep;
use storage_proofs::sector::SectorId;
use storage_proofs::stacked::{generate_replica_id, StackedDrg};
use tempfile::tempfile;

use crate::api::util::as_safe_commitment;
use crate::constants::{
    DefaultPieceHasher,
    MINIMUM_RESERVED_BYTES_FOR_PIECE_IN_FULLY_ALIGNED_SECTOR as MINIMUM_PIECE_SIZE,
};
use crate::error;
use crate::fr32::{write_padded, write_unpadded};
use crate::parameters::public_params;
use crate::pieces::{get_piece_alignment, sum_piece_bytes_with_alignment};
use crate::types::{
    Commitment, PaddedBytesAmount, PieceInfo, PoRepConfig, PoRepProofPartitions, ProverId, Ticket,
    UnpaddedByteIndex, UnpaddedBytesAmount,
};

mod post;
mod seal;
pub(crate) mod util;

pub use self::post::*;
pub use self::seal::*;

pub use crate::pieces::verify_pieces;

/// Unseals the sector at `sealed_path` and returns the bytes for a piece
/// whose first (unpadded) byte begins at `offset` and ends at `offset` plus
/// `num_bytes`, inclusive. Note that the entire sector is unsealed each time
/// this function is called.
#[allow(clippy::too_many_arguments)]
pub fn get_unsealed_range<T: Into<PathBuf> + AsRef<Path>>(
    porep_config: PoRepConfig,
    sealed_path: T,
    output_path: T,
    prover_id: ProverId,
    sector_id: SectorId,
    comm_d: Commitment,
    ticket: Ticket,
    offset: UnpaddedByteIndex,
    num_bytes: UnpaddedBytesAmount,
) -> error::Result<(UnpaddedBytesAmount)> {
    let comm_d =
        as_safe_commitment::<<DefaultPieceHasher as Hasher>::Domain, _>(&comm_d, "comm_d")?;

    let replica_id =
        generate_replica_id::<DefaultTreeHasher, _>(&prover_id, sector_id.into(), &ticket, comm_d);

    let f_in = File::open(sealed_path)?;
    let mut data = Vec::new();
    f_in.take(u64::from(PaddedBytesAmount::from(porep_config)))
        .read_to_end(&mut data)?;

    let f_out = File::create(output_path)?;
    let mut buf_writer = BufWriter::new(f_out);

    let unsealed = StackedDrg::<DefaultTreeHasher, DefaultPieceHasher>::extract_all(
        &public_params(
            PaddedBytesAmount::from(porep_config),
            usize::from(PoRepProofPartitions::from(porep_config)),
        ),
        &replica_id,
        &data,
    )?;

    let written = write_unpadded(&unsealed, &mut buf_writer, offset.into(), num_bytes.into())?;

    Ok(UnpaddedBytesAmount(written as u64))
}

// Takes a piece and the size of it, and generates the commitment for it.
pub fn generate_piece_commitment<T: std::io::Read>(
    unpadded_piece_file: T,
    unpadded_piece_size: UnpaddedBytesAmount,
) -> error::Result<PieceInfo> {
    let mut padded_piece_file = tempfile()?;
    add_piece(
        unpadded_piece_file,
        &mut padded_piece_file,
        unpadded_piece_size,
        &[],
    )
}

/// Write a piece. Returns the `PieceInfo` for this piece.
///
/// The `target` should always be a `BufWriter` or other type of buffered writer.
pub fn add_piece<R, W>(
    source: R,
    mut target: W,
    piece_size: UnpaddedBytesAmount,
    piece_lengths: &[UnpaddedBytesAmount],
) -> error::Result<PieceInfo>
where
    R: Read,
    W: Read + Write + Seek,
{
    ensure!(
        piece_size >= UnpaddedBytesAmount(MINIMUM_PIECE_SIZE),
        "Piece must be at least {} bytes",
        MINIMUM_PIECE_SIZE
    );
    let padded_piece_size: PaddedBytesAmount = piece_size.into();
    ensure!(
        u64::from(padded_piece_size).is_power_of_two(),
        "Bit-padded piece size must be a power of 2 ({:?})",
        padded_piece_size,
    );

    // Calculate alignment.
    let written_bytes = sum_piece_bytes_with_alignment(piece_lengths);
    let piece_alignment = get_piece_alignment(written_bytes, piece_size);
    let bytes_with_alignment = piece_alignment.sum(piece_size);

    let mut written = 0;

    // 1. write left alignment
    {
        let left_bytes: PaddedBytesAmount = piece_alignment.left_bytes.into();
        for _ in 0..u64::from(left_bytes) as usize {
            target.write_all(&[0])?;
        }
        written += u64::from(piece_alignment.left_bytes) as usize;
    }

    // 2. write actual data

    // Save the current position of the target.
    let start = target.seek(SeekFrom::Current(0))?;
    let piece_written = write_padded(source, &mut target)?;
    written += piece_written;

    // 3. calculate piece commitemnt over the data

    // Ensure we build the piece over the data we have actually written.
    let piece_end = target.seek(SeekFrom::Current(0))?;
    let _ = target.seek(SeekFrom::Start(start))?;
    let commitment =
        generate_piece_commitment_bytes_from_source::<DefaultPieceHasher>(&mut target)?;
    // restore position
    let _ = target.seek(SeekFrom::Start(piece_end))?;

    // 4. write right alignment
    {
        let right_bytes: PaddedBytesAmount = piece_alignment.right_bytes.into();
        for _ in 0..u64::from(right_bytes) as usize {
            target.write_all(&[0])?;
        }
        written += u64::from(piece_alignment.right_bytes) as usize;
    }

    ensure!(
        u64::from(bytes_with_alignment) == written as u64,
        "Invalid write: {} != {}",
        u64::from(bytes_with_alignment),
        written,
    );

    Ok(PieceInfo {
        commitment,
        size: piece_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::io::{Seek, SeekFrom, Write};

    use paired::bls12_381::Bls12;
    use rand::{Rng, SeedableRng, XorShiftRng};
    use storage_proofs::fr32::bytes_into_fr;
    use tempfile::NamedTempFile;

    use crate::constants::{SECTOR_SIZE_ONE_KIB, SINGLE_PARTITION_PROOF_LEN};
    use crate::types::{PieceInfo, PoStConfig, SectorSize};

    #[test]
    fn test_verify_seal_fr32_validation() {
        let convertible_to_fr_bytes = [0; 32];
        let out = bytes_into_fr::<Bls12>(&convertible_to_fr_bytes);
        assert!(out.is_ok(), "tripwire");

        let not_convertible_to_fr_bytes = [255; 32];
        let out = bytes_into_fr::<Bls12>(&not_convertible_to_fr_bytes);
        assert!(out.is_err(), "tripwire");

        {
            let result = verify_seal(
                PoRepConfig(SectorSize(SECTOR_SIZE_ONE_KIB), PoRepProofPartitions(2)),
                not_convertible_to_fr_bytes,
                convertible_to_fr_bytes,
                [0; 32],
                SectorId::from(0),
                [0; 32],
                [0; 32],
                &[],
                &[PieceInfo::default()],
            );

            if let Err(err) = result {
                let needle = "Invalid commitment (comm_r)";
                let haystack = format!("{}", err);

                assert!(
                    haystack.contains(needle),
                    format!("\"{}\" did not contain \"{}\"", haystack, needle)
                );
            } else {
                panic!("should have failed comm_r to Fr32 conversion");
            }
        }

        {
            let result = verify_seal(
                PoRepConfig(SectorSize(SECTOR_SIZE_ONE_KIB), PoRepProofPartitions(2)),
                convertible_to_fr_bytes,
                not_convertible_to_fr_bytes,
                [0; 32],
                SectorId::from(0),
                [0; 32],
                [0; 32],
                &[],
                &[],
            );

            if let Err(err) = result {
                let needle = "Invalid commitment (comm_d)";
                let haystack = format!("{}", err);

                assert!(
                    haystack.contains(needle),
                    format!("\"{}\" did not contain \"{}\"", haystack, needle)
                );
            } else {
                panic!("should have failed comm_d to Fr32 conversion");
            }
        }
    }

    #[test]
    fn test_verify_post_fr32_validation() {
        let not_convertible_to_fr_bytes = [255; 32];
        let out = bytes_into_fr::<Bls12>(&not_convertible_to_fr_bytes);
        assert!(out.is_err(), "tripwire");
        let mut replicas = BTreeMap::new();
        replicas.insert(
            1.into(),
            PublicReplicaInfo::new(not_convertible_to_fr_bytes),
        );

        let result = verify_post(
            PoStConfig(SectorSize(SECTOR_SIZE_ONE_KIB)),
            &[0; 32],
            &vec![0; SINGLE_PARTITION_PROOF_LEN],
            &replicas,
        );

        if let Err(err) = result {
            let needle = "Invalid commitment (comm_r)";
            let haystack = format!("{}", err);

            assert!(
                haystack.contains(needle),
                format!("\"{}\" did not contain \"{}\"", haystack, needle)
            );
        } else {
            panic!("should have failed comm_r to Fr32 conversion");
        }
    }

    #[test]
    #[ignore]
    fn test_seal_lifecycle() -> Result<(), failure::Error> {
        pretty_env_logger::try_init().ok();

        let rng = &mut XorShiftRng::from_seed([0x3dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

        let sector_size = SECTOR_SIZE_ONE_KIB;

        let number_of_bytes_in_piece =
            UnpaddedBytesAmount::from(PaddedBytesAmount(sector_size.clone()));

        let piece_bytes: Vec<u8> = (0..number_of_bytes_in_piece.0)
            .map(|_| rand::random::<u8>())
            .collect();

        let mut piece_file = NamedTempFile::new()?;
        piece_file.write_all(&piece_bytes)?;
        piece_file.as_file_mut().sync_all()?;
        piece_file.as_file_mut().seek(SeekFrom::Start(0))?;

        let mut staged_sector_file = NamedTempFile::new()?;
        let piece_info = add_piece(
            &mut piece_file,
            &mut staged_sector_file,
            number_of_bytes_in_piece,
            &[],
        )?;

        let piece_infos = vec![piece_info];

        let sealed_sector_file = NamedTempFile::new()?;
        let config = PoRepConfig(SectorSize(sector_size.clone()), PoRepProofPartitions(2));

        let cache_dir = tempfile::tempdir().unwrap();
        let prover_id = rng.gen();
        let ticket = rng.gen();
        let seed = rng.gen();
        let sector_id = SectorId::from(12);

        let pre_commit_output = seal_pre_commit(
            config,
            cache_dir.path(),
            &staged_sector_file.path(),
            &sealed_sector_file.path(),
            prover_id,
            sector_id,
            ticket,
            &piece_infos,
        )?;

        let comm_d = pre_commit_output.comm_d.clone();
        let comm_r = pre_commit_output.comm_r.clone();

        let commit_output = seal_commit(
            config,
            cache_dir.path(),
            prover_id,
            sector_id,
            ticket,
            seed,
            pre_commit_output,
            &piece_infos,
        )?;

        let verified = verify_seal(
            config,
            comm_r,
            comm_d,
            prover_id,
            sector_id,
            ticket,
            seed,
            &commit_output.proof,
            &piece_infos,
        )?;
        assert!(verified, "failed to verify valid seal");

        Ok(())
    }
}
