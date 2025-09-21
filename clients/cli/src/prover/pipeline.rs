//! Proving pipeline that orchestrates the full proving process

use super::engine::ProvingEngine;
use super::input::InputParser;
use super::types::ProverError;
use crate::analytics::track_verification_failed;
use crate::environment::Environment;
use crate::task::Task;
use futures::stream::{StreamExt, TryStreamExt};
use nexus_sdk::stwo::seq::Proof;
use sha3::{Digest, Keccak256};

/// Orchestrates the complete proving pipeline
pub struct ProvingPipeline;

impl ProvingPipeline {
    /// Execute authenticated proving for a task
    pub async fn prove_authenticated(
        task: &Task,
        environment: &Environment,
        client_id: &str,
        num_workers: &usize,
    ) -> Result<(Vec<Proof>, String, Vec<String>), ProverError> {
        match task.program_id.as_str() {
            "fib_input_initial" => {
                Self::prove_fib_task(task, environment, client_id, num_workers).await
            }
            _ => Err(ProverError::MalformedTask(format!(
                "Unsupported program ID: {}",
                task.program_id
            ))),
        }
    }

    /// Process fibonacci proving task with multiple inputs
    async fn prove_fib_task(
        task: &Task,
        environment: &Environment,
        client_id: &str,
        num_workers: &usize,
    ) -> Result<(Vec<Proof>, String, Vec<String>), ProverError> {
        let all_inputs = task.all_inputs();

        if all_inputs.is_empty() {
            return Err(ProverError::MalformedTask(
                "No inputs provided for task".to_string(),
            ));
        }

        let mut proof_hashes = Vec::new();
        let mut all_proofs: Vec<Proof> = Vec::new();

        let all_inputs: Vec<Vec<u8>> = all_inputs.to_vec();

        let stream = futures::stream::iter(all_inputs.into_iter().enumerate().map(
            |(input_index, input_data)| {
                async move {
                    // Step 1: Parse and validate input
                    let inputs = InputParser::parse_triple_input(&input_data)?;

                    // Step 2: Generate and verify proof
                    let proof =
                        ProvingEngine::prove_and_validate(&inputs, task, environment, client_id)
                            .await
                            .map_err(|e| {
                                match e {
                                    ProverError::Stwo(_) | ProverError::GuestProgram(_) => {
                                        // Track verification failure
                                        let error_msg = format!("Input {}: {}", input_index, e);
                                        tokio::spawn(track_verification_failed(
                                            task.clone(),
                                            error_msg.clone(),
                                            environment.clone(),
                                            client_id.to_string(),
                                        ));
                                        e
                                    }
                                    _ => e,
                                }
                            })?;

                    // Step 3: Generate proof hash
                    let proof_hash = Self::generate_proof_hash(&proof);
                    Ok::<(Proof, String, usize), ProverError>((proof, proof_hash, input_index))
                }
            },
        ));

        let results: Vec<(Proof, String, usize)> =
            match stream.buffer_unordered(*num_workers).try_collect().await {
                Ok(res) => res,
                Err(e) => {
                    return Err(e);
                }
            };

        let mut results = results;
        results.sort_by_key(|(_, _, index)| *index);

        for (proof, hash, _) in results {
            proof_hashes.push(hash);
            all_proofs.push(proof);
        }

        let final_proof_hash = Self::combine_proof_hashes(task, &proof_hashes);

        Ok((all_proofs, final_proof_hash, proof_hashes))
    }

    /// Generate hash for a proof
    fn generate_proof_hash(proof: &Proof) -> String {
        let proof_bytes = postcard::to_allocvec(proof).expect("Failed to serialize proof");
        format!("{:x}", Keccak256::digest(&proof_bytes))
    }

    /// Combine multiple proof hashes based on task type
    fn combine_proof_hashes(task: &Task, proof_hashes: &[String]) -> String {
        match task.task_type {
            crate::nexus_orchestrator::TaskType::AllProofHashes
            | crate::nexus_orchestrator::TaskType::ProofHash => {
                Task::combine_proof_hashes(proof_hashes)
            }
            _ => proof_hashes.first().cloned().unwrap_or_default(),
        }
    }
}