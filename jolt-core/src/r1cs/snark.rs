use std::collections::HashMap;

use common::path::JoltPaths;
use spartan2::{
  traits::{snark::RelaxedR1CSSNARKTrait, Group},
  SNARK, errors::SpartanError,
};

use bellpepper_core::{Circuit, ConstraintSystem, LinearCombination, SynthesisError, Variable, Index};
use ff::PrimeField;
use ruint::aliases::U256;


use circom_scotia::r1cs::CircomConfig;

const WTNS_GRAPH_BYTES: &[u8] = include_bytes!("./graph.bin");

#[derive(Clone, Debug, Default)]
pub struct JoltCircuit<F: PrimeField<Repr=[u8; 32]>> {
  width: usize,
  c: usize,
  num_steps: usize,
  pc_start_addr: F,
  inputs: Vec<Vec<F>>,
  // prog_a_rw: Vec<F>, 
  // prog_v_rw: Vec<F>, 
  // prog_t_reads: Vec<F>, 
  // memreg_a_rw: Vec<F>, 
  // memreg_v_reads: Vec<F>, 
  // memreg_v_writes: Vec<F>, 
  // memreg_t_reads: Vec<F>,
  // chunks_x: Vec<F>, 
  // chunks_y: Vec<F>, 
  // chunks_query: Vec<F>, 
  // lookup_outputs: Vec<F>, 
  // op_flags: Vec<F>,
}

impl<F: PrimeField<Repr=[u8;32]>> JoltCircuit<F> {
  pub fn new_from_inputs(W: usize, c: usize, num_steps: usize, PC_START_ADDR: F, inputs: Vec<Vec<F>>) -> Self {
    // TODO(sragss): What is W?
    JoltCircuit{
      width: W,
      c: c,
      num_steps: num_steps,
      pc_start_addr: PC_START_ADDR,
      inputs: inputs,
    }
  }

  pub fn all_zeros(W: usize, c: usize, N: usize) -> Self {
    JoltCircuit{
      width: W,
      c: c,
      num_steps: N,
      pc_start_addr: 0.into(),
      inputs: vec![
        vec![F::ZERO; N * 6], // TODO: change this to just N * 1 as prog code address is de-duplicated
        vec![F::ZERO; N * 6],
        vec![F::ZERO; N * 6], // same for t_reads
        vec![F::ZERO; N * 3+(W/8)], 
        vec![F::ZERO; N * 3+(W/8)],
        vec![F::ZERO; N * 3+(W/8)],
        vec![F::ZERO; N * 3+(W/8)],
        vec![F::ZERO; N * c],
        vec![F::ZERO; N * c],
        vec![F::ZERO; N * c],
        vec![F::ZERO; N],
        vec![F::ZERO; N * 15],
      ],
    }
  }
}

impl<F: PrimeField<Repr = [u8; 32]>> Circuit<F> for JoltCircuit<F> {
  #[tracing::instrument(skip_all, name = "JoltCircuit::synthesize")]
  fn synthesize<CS: ConstraintSystem<F>>(self, cs: &mut CS) -> Result<(), SynthesisError> {
    // TODO(sragss): These paths should not be hardcoded.
    let r1cs_path = JoltPaths::r1cs_path();
    let wtns_path = JoltPaths::witness_generator_path();

    // let cfg = CircomConfig::new(self.witness_generator_path.clone(), self.r1cs_path.clone()).unwrap();
    let cfg = CircomConfig::new(wtns_path.clone(), r1cs_path.clone()).unwrap();

    let variable_names = [
      "prog_a_rw", 
      "prog_v_rw", 
      "memreg_a_rw", 
      "memreg_v_reads", 
      "memreg_v_writes", 
      "chunks_x", 
      "chunks_y", 
      "chunks_query", 
      "lookup_output", 
      "op_flags"
    ];

    assert_eq!(self.num_steps, self.inputs[0].len()); 
    let TRACE_LEN = self.inputs[0].len();
    let NUM_STEPS = TRACE_LEN;

    // for variable [v], step_inputs[v][j] is the variable input for step j
    let span = tracing::span!(tracing::Level::INFO, "inner_vec_mapping");
    let _guard = span.enter();
    let inputs_chunked : Vec<Vec<_>> = self.inputs
      .into_iter()
      .map(|inner_vec| inner_vec.chunks(inner_vec.len()/TRACE_LEN).map(|chunk| chunk.to_vec()).collect())
      .collect();
    drop(_guard);
    drop(span);

    let mut current_state = [F::from(0), self.pc_start_addr];

    let compute_witness_span = tracing::span!(tracing::Level::INFO, "compute_witness_loop");
    let _compute_witness_guard = compute_witness_span.enter();

    let graph = witness::init_graph(WTNS_GRAPH_BYTES).unwrap();
    for i in 0..NUM_STEPS {
      let step_inputs = inputs_chunked.iter().map(|v| v[i].clone()).collect::<Vec<_>>();

      let mut input: Vec<(String, Vec<F>)> = variable_names
        .iter()
        .zip(step_inputs.into_iter())
        .map(|(name, input)| (name.to_string(), input))
        .collect();

      input.push(("input_state".to_string(), current_state.to_vec()));

      let span = tracing::span!(tracing::Level::INFO, "calculate_witness");
      let _guard = span.enter();




      // TODO(sragss): idt this works.
      let rs_wtns_span = tracing::span!(tracing::Level::INFO, "rs_wtns");
      let rs_wtns_guard = rs_wtns_span.enter();
      let input_converted: HashMap<String, Vec<U256>> = input
          .into_iter()
          .map(|(k, v)| {
            (k, v.into_iter().map(|x| {
              let bytes  = x.to_repr();
              let bytes: &[u8] = bytes.as_ref();
              let bi: [u8; 32] = bytes.try_into().unwrap();
              U256::from_le_bytes(bi)
            }).collect())
          })
          .collect();
      let uint_jolt_witness = witness::calculate_witness(input_converted, &graph).unwrap();
      drop(rs_wtns_guard);
      drop(rs_wtns_span);

      let jolt_witness: Vec<F> = uint_jolt_witness.into_iter().map(|x| {
        let bytes: [u8; 32] = x.to_le_bytes().try_into().expect("should be 256 bits");
        F::from_repr(bytes).unwrap()
      }).collect::<Vec<_>>();
      drop(_guard);
      drop(span);

      current_state = [jolt_witness[1], jolt_witness[2]]; 

      let span = tracing::span!(tracing::Level::INFO, "circom_scotia::synthesize");
      let _guard = span.enter();
      let _ = circom_scotia::synthesize(
          &mut cs.namespace(|| format!("jolt_step_{}", i)),
          cfg.r1cs.clone(),
          Some(jolt_witness),
      )
      .unwrap();
      drop(_guard);
      drop(span);
    }
    drop(_compute_witness_guard);
    drop(compute_witness_span);

    /* Consistency constraints between steps: 
    - Note that all steps use the same CS::one() variable as the constant 
    - The only task then is to ensure that the input of each step is the output of the previous step  

    Every variable is allocated into Aux and is in the following order: 
    Aux: [out0, in0, aux0, ..., out_i, in_i, aux_i ...]
     */

    let NUM_VARS_PER_STEP = cfg.r1cs.num_variables - 1; // exclude the constant 1
    let STATE_SIZE = 2; 
    let span = tracing::span!(tracing::Level::INFO, "constraint_system::enforce_io_consistency");
    let _guard = span.enter();
    for i in 0..NUM_STEPS-1 {
      let out_start_index = NUM_VARS_PER_STEP * i;
      let in_start_next = NUM_VARS_PER_STEP * (i+1) + STATE_SIZE;
      for j in 0..STATE_SIZE {
        cs.enforce(
          || format!("io consistency constraint {}, {}", i, j),
          |_| LinearCombination::<F>::zero() + (F::from(1), CS::one()), 
          |_| LinearCombination::<F>::zero() + (F::from(1), Variable::new_unchecked(Index::Aux(in_start_next+j))), 
          |_| LinearCombination::<F>::zero() + (F::from(1), Variable::new_unchecked(Index::Aux(out_start_index+j))), 
        );
      }
    }
    drop(_guard);
    drop(span);
    Ok(())
  }
}


#[derive(Clone, Debug, Default)]
pub struct JoltSkeleton<F: PrimeField<Repr = [u8; 32]>> {
  num_steps: usize,
  _phantom: std::marker::PhantomData<F>,
}

impl<F: PrimeField<Repr = [u8; 32]>> JoltSkeleton<F> {
  pub fn from_num_steps(num_steps: usize) -> Self {
    JoltSkeleton::<F>{
      num_steps: num_steps,
      _phantom: std::marker::PhantomData,
    }
  }
}

impl<F: PrimeField<Repr = [u8; 32]>> Circuit<F> for JoltSkeleton<F> {
  #[tracing::instrument(skip_all, name = "JoltSkeleton::synthesize")]
  fn synthesize<CS: ConstraintSystem<F>>(self, cs: &mut CS) -> Result<(), SynthesisError> {
    // TODO(sragss): These paths should not be hardcoded.
    let circuit_dir = JoltPaths::circuit_artifacts_path();
    let r1cs_path = circuit_dir.join("jolt_single_step.r1cs");
    let wtns_path = circuit_dir.join("jolt_single_step_js/jolt_single_step.wasm");

    // let cfg = CircomConfig::new(self.witness_generator_path.clone(), self.r1cs_path.clone()).unwrap();
    let load_cfg_span = tracing::span!(tracing::Level::INFO, "load_cfg");
    let _load_cfg_guard = load_cfg_span.enter();
    let cfg = CircomConfig::new(wtns_path.clone(), r1cs_path.clone()).unwrap();
    drop(_load_cfg_guard);
    drop(load_cfg_span);

    let NUM_STEPS = self.num_steps;
    let NUM_VARS_PER_STEP = cfg.r1cs.num_variables - 1; // exclude the constant 1

    let synthesize_span = tracing::span!(tracing::Level::INFO, "circom_scotia_synthesize");
    let _synthesize_guard = synthesize_span.enter();
    for i in 0..1 {
      let _ = circom_scotia::synthesize(
          &mut cs.namespace(|| format!("jolt_step_{}", i)),
          cfg.r1cs.clone(),
          None,
      )
      .unwrap();
    }
    drop(_synthesize_guard);
    drop(synthesize_span);

    let NUM_VARS_PER_STEP = cfg.r1cs.num_variables - 1; // exclude the constant 1
    let STATE_SIZE = 2; 
    let span = tracing::span!(tracing::Level::INFO, "constraint_loop");
    let _guard = span.enter();
    // for i in 0..NUM_STEPS-1 {
    //   let out_start_index = NUM_VARS_PER_STEP * i;
    //   let in_start_next = NUM_VARS_PER_STEP * (i+1) + STATE_SIZE;
    //   for j in 0..STATE_SIZE {
    //     cs.enforce(
    //       || format!("io consistency constraint {}, {}", i, j),
    //       |_| LinearCombination::<F>::zero() + (F::from(1), CS::one()), 
    //       |_| LinearCombination::<F>::zero() + (F::from(1), Variable::new_unchecked(Index::Aux(in_start_next+j))), 
    //       |_| LinearCombination::<F>::zero() + (F::from(1), Variable::new_unchecked(Index::Aux(out_start_index+j))), 
    //     );
    //   }
    // }
    drop(_guard);
    drop(span);
    Ok(())
  }
}


#[tracing::instrument(skip_all, name = "JoltCircuit::run_jolt_spartan_with_circuit")]
// pub fn run_jolt_spartan_with_circuit<G: Group, S: RelaxedR1CSSNARKTrait<G>>(circuit: JoltCircuit<<G as Group>::Scalar>) -> Result<Vec<<G as Group>::Scalar>, SpartanError> {
pub fn run_jolt_spartan_with_circuit<G: Group<Scalar = F>, S: RelaxedR1CSSNARKTrait<G>, F: PrimeField<Repr = [u8; 32]>>(circuit: JoltCircuit<F>) -> Result<(), SpartanError> {
  let num_steps = circuit.inputs[0].len(); 
  // produce keys
  // let circuit_clone = circuit.clone();
  let skeleton_circuit = JoltSkeleton::<G::Scalar>::from_num_steps(num_steps);
  let span = tracing::span!(tracing::Level::INFO, "setup SNARK");
  let _guard = span.enter();
  // let (pk, vk) = SNARK::<G, S, JoltSkeleton<<G as Group>::Scalar>>::setup(skeleton_circuit).unwrap();
  let (pk, vk) = SNARK::<G, S, JoltSkeleton<<G as Group>::Scalar>>::setup_uniform(skeleton_circuit, num_steps).unwrap();
  drop(_guard);
  drop(span);

  // produce a SNARK
  let span = tracing::span!(tracing::Level::INFO, "produce a SNARK");
  let _guard = span.enter();
  let res = SNARK::prove(&pk, circuit);
  assert!(res.is_ok());
  let snark = res.unwrap();
  drop(_guard);
  drop(span);

  // verify the SNARK
  let span = tracing::span!(tracing::Level::INFO, "verify the SNARK");
  let _guard = span.enter();
  let res = snark.verify(&vk, &[]);
  drop(_guard);
  drop(span);
  res 
}


mod test {
  use spartan2::{
    provider::bn256_grumpkin::bn256,
    traits::Group,
    SNARK,
  };
  use super::{JoltCircuit, run_jolt_spartan_with_circuit};

  // #[test]
  // fn test_jolt_snark() {
  //   super::run_jolt_spartan();
  // }

  #[test]
  fn test_all_zeros() {
    type G1 = bn256::Point;
    type EE = spartan2::provider::ipa_pc::EvaluationEngine<G1>;
    type S = spartan2::spartan::snark::RelaxedR1CSSNARK<G1, EE>;

    type F = <G1 as Group>::Scalar;

    let N = 1; 
    let W = 64;
    let c = 6;

    let prog_a_rw = vec![F::zero(); N * 6];
    let prog_v_rw = vec![F::zero(); N * 6];
    let prog_t_reads = vec![F::zero(); N * 6];
    let memreg_a_rw = vec![F::zero(); N * 3+(W/8)];
    let memreg_v_reads = vec![F::zero(); N * 3+(W/8)];
    let memreg_v_writes = vec![F::zero(); N * 3+(W/8)];
    let memreg_t_reads = vec![F::zero(); N * c];
    let chunks_x = vec![F::zero(); N * c];
    let chunks_y=  vec![F::zero(); N * c];
    let chunks_query = vec![F::zero(); N];
    let lookup_outputs = vec![F::zero(); N];
    let op_flags = vec![F::zero(); N * 15];

    let inputs = vec![
      prog_a_rw,
      prog_v_rw,
      prog_t_reads,
      memreg_a_rw,
      memreg_v_reads, 
      memreg_v_writes,
      memreg_t_reads, 
      chunks_x, 
      chunks_y,
      chunks_query,
      lookup_outputs,
      op_flags,
    ];

    unimplemented!("Need to commit a src/r1cs/test-artifacts/ <witness generator> / <r1cs>");
    // let jolt_circuit = JoltCircuit::<<G1 as Group>::Scalar>::new_from_inputs(32, 3, inputs);
    // let res_verifier = run_jolt_spartan_with_circuit::<G1, S>(jolt_circuit);
    // assert!(res_verifier.is_ok());
  }

  #[test]
  fn test_add() {
    type G1 = bn256::Point;
    type EE = spartan2::provider::ipa_pc::EvaluationEngine<G1>;
    type S = spartan2::spartan::snark::RelaxedR1CSSNARK<G1, EE>;

    type F = <G1 as Group>::Scalar;

    let N = 1; 
    let W = 64;
    let c = 6;

    let mut prog_a_rw = vec![F::zero(); N * 6];
    let mut prog_v_rw = vec![F::zero(); N * 6];
    let mut prog_t_reads = vec![F::zero(); N * 6];
    let mut memreg_a_rw = vec![F::zero(); N * 3+(W/8)];
    let mut memreg_v_reads = vec![F::zero(); N * 3+(W/8)];
    let mut memreg_v_writes = vec![F::zero(); N * 3+(W/8)];
    let mut memreg_t_reads = vec![F::zero(); N * c];
    let mut chunks_x = vec![F::zero(); N * c];
    let mut chunks_y=  vec![F::zero(); N * c];
    let mut chunks_query = vec![F::zero(); c];
    let mut lookup_outputs = vec![F::zero(); N];
    let mut op_flags = vec![F::zero(); N * 15];

    // prog_v_rw: (ADD, rs1= 1, rs2=2, rd=3, imm=0, op_flags_packed=12)
    prog_v_rw = vec![F::from(10), F::from(1), F::from(2), F::from(3), F::from(0), F::from(128)];

    // memreg_a_rw: (rs1=1, rs2=2, rd=3)
    memreg_a_rw[0] = F::from(1);
    memreg_a_rw[1] = F::from(2);
    memreg_a_rw[10] = F::from(3);

    // suppose rs1 stores 1, rs2 stores 6, rd has 0
    memreg_v_reads[0] = F::from(1);
    memreg_v_reads[1] = F::from(6);
    memreg_v_reads[10] = F::from(0);

    memreg_v_writes[0] = F::from(1);
    memreg_v_writes[1] = F::from(6);
    memreg_v_writes[10] = F::from(7);

    chunks_x[c-1] = F::from(0);
    chunks_y[c-1] = F::from(0);
    chunks_query[c-1] = F::from(0);

    op_flags[7] = F::from(1); // is_add_instr

    let inputs = vec![
      prog_a_rw,
      prog_v_rw,
      prog_t_reads,
      memreg_a_rw,
      memreg_v_reads, 
      memreg_v_writes,
      memreg_t_reads, 
      chunks_x, 
      chunks_y,
      chunks_query,
      lookup_outputs,
      op_flags,
    ];

    unimplemented!("Need to commit a src/r1cs/test-artifacts/ <witness generator> / <r1cs>");
    // let jolt_circuit = JoltCircuit::<<G1 as Group>::Scalar>::new_from_inputs(64, 6, inputs);
    // let res_verifier = run_jolt_spartan_with_circuit::<G1, S>(jolt_circuit);
    // assert!(res_verifier.is_ok());
  }

  #[test]
  fn test_add_should_fail() {
    type G1 = bn256::Point;
    type EE = spartan2::provider::ipa_pc::EvaluationEngine<G1>;
    type S = spartan2::spartan::snark::RelaxedR1CSSNARK<G1, EE>;

    type F = <G1 as Group>::Scalar;

    let N = 1; 
    let W = 64;
    let c = 6;

    let mut prog_a_rw = vec![F::zero(); N * 6];
    let mut prog_v_rw = vec![F::zero(); N * 6];
    let mut prog_t_reads = vec![F::zero(); N * 6];
    let mut memreg_a_rw = vec![F::zero(); N * 3+(W/8)];
    let mut memreg_v_reads = vec![F::zero(); N * 3+(W/8)];
    let mut memreg_v_writes = vec![F::zero(); N * 3+(W/8)];
    let mut memreg_t_reads = vec![F::zero(); N * c];
    let mut chunks_x = vec![F::zero(); N * c];
    let mut chunks_y=  vec![F::zero(); N * c];
    let mut chunks_query = vec![F::zero(); c];
    let mut lookup_outputs = vec![F::zero(); N];
    let mut op_flags = vec![F::zero(); N * 15];

    prog_v_rw = vec![F::from(10), F::from(1), F::from(2), F::from(3), F::from(0), F::from(128)];

    // ERROR: this should be rs1 = 1
    memreg_a_rw[0] = F::from(16);
    memreg_a_rw[1] = F::from(2);
    memreg_a_rw[10] = F::from(3);

    memreg_v_reads[0] = F::from(1);
    memreg_v_reads[1] = F::from(6);
    memreg_v_reads[10] = F::from(0);

    memreg_v_writes[0] = F::from(1);
    memreg_v_writes[1] = F::from(6);
    memreg_v_writes[10] = F::from(7);

    chunks_x[c-1] = F::from(0);
    chunks_y[c-1] = F::from(0);
    chunks_query[c-1] = F::from(0);

    op_flags[7] = F::from(1); // is_add_instr

    let inputs = vec![
      prog_a_rw,
      prog_v_rw,
      prog_t_reads,
      memreg_a_rw,
      memreg_v_reads, 
      memreg_v_writes,
      memreg_t_reads, 
      chunks_x, 
      chunks_y,
      chunks_query,
      lookup_outputs,
      op_flags,
    ];

    unimplemented!("Need to commit a src/r1cs/test-artifacts/ <witness generator> / <r1cs>");
    // let jolt_circuit = JoltCircuit::<<G1 as Group>::Scalar>::new_from_inputs(64, 6, inputs);
    // let res_verifier = run_jolt_spartan_with_circuit::<G1, S>(jolt_circuit);
    // assert!(res_verifier.is_err());
  }
}