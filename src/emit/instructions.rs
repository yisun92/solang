// SPDX-License-Identifier: Apache-2.0

use crate::codegen::cfg::{ControlFlowGraph, Instr, InternalCallTy, ReturnCode};
use crate::codegen::Expression;
use crate::emit::binary::Binary;
use crate::emit::cfg::{create_block, BasicBlock, Work};
use crate::emit::expression::expression;
use crate::emit::TargetRuntime;
use crate::sema::ast::{Contract, Namespace, RetrieveType, Type};
use crate::Target;
use inkwell::types::BasicType;
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, CallableValue, FunctionValue, IntValue,
};
use inkwell::{AddressSpace, IntPredicate};
use num_traits::ToPrimitive;
use std::collections::{HashMap, VecDeque};

pub(super) fn process_instruction<'a, T: TargetRuntime<'a> + ?Sized>(
    target: &mut T,
    ins: &Instr,
    bin: &Binary<'a>,
    w: &mut Work<'a>,
    function: FunctionValue<'a>,
    ns: &Namespace,
    cfg: &ControlFlowGraph,
    work: &mut VecDeque<Work<'a>>,
    blocks: &mut HashMap<usize, BasicBlock<'a>>,
    contract: &Contract,
) {
    match ins {
        Instr::Nop => (),
        Instr::Return { value } if value.is_empty() => {
            bin.builder
                .build_return(Some(&bin.return_values[&ReturnCode::Success]));
        }
        Instr::Return { value } => {
            let returns_offset = cfg.params.len();
            for (i, val) in value.iter().enumerate() {
                let arg = function.get_nth_param((returns_offset + i) as u32).unwrap();
                let retval = expression(target, bin, val, &w.vars, function, ns);

                bin.builder.build_store(arg.into_pointer_value(), retval);
            }

            bin.builder
                .build_return(Some(&bin.return_values[&ReturnCode::Success]));
        }
        Instr::Set { res, expr, .. } => {
            if let Expression::Undefined(expr_type) = expr {
                // If the variable has been declared as undefined, but we can
                // initialize it with a default value
                if let Some(default_expr) = expr_type.default(ns) {
                    w.vars.get_mut(res).unwrap().value =
                        expression(target, bin, &default_expr, &w.vars, function, ns);
                }
            } else {
                w.vars.get_mut(res).unwrap().value =
                    expression(target, bin, expr, &w.vars, function, ns);
            }
        }
        Instr::Branch { block: dest } => {
            let pos = bin.builder.get_insert_block().unwrap();

            let bb = add_or_retrieve_block(*dest, pos, bin, function, blocks, work, w, cfg, ns);

            bin.builder.position_at_end(pos);
            bin.builder.build_unconditional_branch(bb);
        }
        Instr::Store { dest, data } => {
            let value_ref = expression(target, bin, data, &w.vars, function, ns);
            let dest_ref =
                expression(target, bin, dest, &w.vars, function, ns).into_pointer_value();
            bin.builder.build_store(dest_ref, value_ref);
        }
        Instr::BranchCond {
            cond,
            true_block: true_,
            false_block: false_,
        } => {
            let cond = expression(target, bin, cond, &w.vars, function, ns);

            let pos = bin.builder.get_insert_block().unwrap();

            let bb_true =
                add_or_retrieve_block(*true_, pos, bin, function, blocks, work, w, cfg, ns);

            let bb_false =
                add_or_retrieve_block(*false_, pos, bin, function, blocks, work, w, cfg, ns);

            bin.builder.position_at_end(pos);
            bin.builder
                .build_conditional_branch(cond.into_int_value(), bb_true, bb_false);
        }
        Instr::LoadStorage { res, ty, storage } => {
            let mut slot = expression(target, bin, storage, &w.vars, function, ns).into_int_value();

            w.vars.get_mut(res).unwrap().value =
                target.storage_load(bin, ty, &mut slot, function, ns);
        }
        Instr::ClearStorage { ty, storage } => {
            let mut slot = expression(target, bin, storage, &w.vars, function, ns).into_int_value();

            target.storage_delete(bin, ty, &mut slot, function, ns);
        }
        Instr::SetStorage { ty, value, storage } => {
            let value = expression(target, bin, value, &w.vars, function, ns);

            let mut slot = expression(target, bin, storage, &w.vars, function, ns).into_int_value();

            target.storage_store(bin, ty, true, &mut slot, value, function, ns);
        }
        Instr::SetStorageBytes {
            storage,
            value,
            offset,
        } => {
            let value = expression(target, bin, value, &w.vars, function, ns);

            let slot = expression(target, bin, storage, &w.vars, function, ns).into_int_value();
            let offset = expression(target, bin, offset, &w.vars, function, ns).into_int_value();

            target.set_storage_bytes_subscript(bin, function, slot, offset, value.into_int_value());
        }
        Instr::PushStorage {
            res,
            ty,
            storage,
            value,
        } => {
            let val = value
                .as_ref()
                .map(|expr| expression(target, bin, expr, &w.vars, function, ns));
            let slot = expression(target, bin, storage, &w.vars, function, ns).into_int_value();

            w.vars.get_mut(res).unwrap().value =
                target.storage_push(bin, function, ty, slot, val, ns);
        }
        Instr::PopStorage { res, ty, storage } => {
            let slot = expression(target, bin, storage, &w.vars, function, ns).into_int_value();

            let value = target.storage_pop(bin, function, ty, slot, res.is_some(), ns);

            if let Some(res) = res {
                w.vars.get_mut(res).unwrap().value = value.unwrap();
            }
        }
        Instr::PushMemory {
            res,
            ty,
            array,
            value,
        } => {
            let arr = w.vars[array].value;

            let llvm_ty = bin.llvm_type(ty, ns);
            let elem_ty = ty.array_elem();

            // Calculate total size for reallocation
            let llvm_elem_ty = bin.llvm_field_ty(&elem_ty, ns);
            let elem_size = llvm_elem_ty
                .size_of()
                .unwrap()
                .const_cast(bin.context.i32_type(), false);
            let len = bin.vector_len(arr);
            let new_len =
                bin.builder
                    .build_int_add(len, bin.context.i32_type().const_int(1, false), "");
            let vec_size = bin
                .module
                .get_struct_type("struct.vector")
                .unwrap()
                .size_of()
                .unwrap()
                .const_cast(bin.context.i32_type(), false);
            let size = bin.builder.build_int_mul(elem_size, new_len, "");
            let size = bin.builder.build_int_add(size, vec_size, "");

            let realloc_size = if ns.target == Target::Solana {
                bin.builder
                    .build_int_z_extend(size, bin.context.i64_type(), "")
            } else {
                size
            };

            // Reallocate and reassign the array pointer
            let new = bin
                .builder
                .build_call(
                    bin.module.get_function("__realloc").unwrap(),
                    &[
                        bin.builder
                            .build_pointer_cast(
                                arr.into_pointer_value(),
                                bin.context.i8_type().ptr_type(AddressSpace::Generic),
                                "a",
                            )
                            .into(),
                        realloc_size.into(),
                    ],
                    "",
                )
                .try_as_basic_value()
                .left()
                .unwrap()
                .into_pointer_value();
            let dest = bin.builder.build_pointer_cast(
                new,
                llvm_ty.ptr_type(AddressSpace::Generic),
                "dest",
            );
            w.vars.get_mut(array).unwrap().value = dest.into();

            // Store the value into the last element
            let slot_ptr = unsafe {
                bin.builder.build_gep(
                    dest,
                    &[
                        bin.context.i32_type().const_zero(),
                        bin.context.i32_type().const_int(2, false),
                        bin.builder.build_int_mul(len, elem_size, ""),
                    ],
                    "data",
                )
            };
            let value = expression(target, bin, value, &w.vars, function, ns);
            let elem_ptr = bin.builder.build_pointer_cast(
                slot_ptr,
                llvm_elem_ty.ptr_type(AddressSpace::Generic),
                "element pointer",
            );
            let value = if elem_ty.is_fixed_reference_type() {
                w.vars.get_mut(res).unwrap().value = elem_ptr.into();
                bin.builder.build_load(value.into_pointer_value(), "elem")
            } else {
                w.vars.get_mut(res).unwrap().value = value;
                value
            };
            bin.builder.build_store(elem_ptr, value);

            // Update the len and size field of the vector struct
            let len_ptr = unsafe {
                bin.builder.build_gep(
                    dest,
                    &[
                        bin.context.i32_type().const_zero(),
                        bin.context.i32_type().const_zero(),
                    ],
                    "len",
                )
            };
            let len_field = bin.builder.build_pointer_cast(
                len_ptr,
                bin.context.i32_type().ptr_type(AddressSpace::Generic),
                "len field",
            );
            bin.builder.build_store(len_field, new_len);

            let size_ptr = unsafe {
                bin.builder.build_gep(
                    dest,
                    &[
                        bin.context.i32_type().const_zero(),
                        bin.context.i32_type().const_int(1, false),
                    ],
                    "size",
                )
            };
            let size_field = bin.builder.build_pointer_cast(
                size_ptr,
                bin.context.i32_type().ptr_type(AddressSpace::Generic),
                "size field",
            );
            bin.builder.build_store(size_field, new_len);
        }
        Instr::PopMemory { res, ty, array } => {
            let a = w.vars[array].value.into_pointer_value();
            let len = unsafe {
                bin.builder.build_gep(
                    a,
                    &[
                        bin.context.i32_type().const_zero(),
                        bin.context.i32_type().const_zero(),
                    ],
                    "a_len",
                )
            };
            let len = bin.builder.build_load(len, "a_len").into_int_value();

            // First check if the array is empty
            let is_array_empty = bin.builder.build_int_compare(
                IntPredicate::EQ,
                len,
                bin.context.i32_type().const_zero(),
                "is_array_empty",
            );
            let error = bin.context.append_basic_block(function, "error");
            let pop = bin.context.append_basic_block(function, "pop");
            bin.builder
                .build_conditional_branch(is_array_empty, error, pop);

            bin.builder.position_at_end(error);
            target.assert_failure(
                bin,
                bin.context
                    .i8_type()
                    .ptr_type(AddressSpace::Generic)
                    .const_null(),
                bin.context.i32_type().const_zero(),
            );

            bin.builder.position_at_end(pop);
            let llvm_ty = bin.llvm_type(ty, ns);

            let elem_ty = ty.array_elem();
            let llvm_elem_ty = bin.llvm_field_ty(&elem_ty, ns);

            // Calculate total size for reallocation
            let elem_size = llvm_elem_ty
                .size_of()
                .unwrap()
                .const_cast(bin.context.i32_type(), false);
            let new_len =
                bin.builder
                    .build_int_sub(len, bin.context.i32_type().const_int(1, false), "");
            let vec_size = bin
                .module
                .get_struct_type("struct.vector")
                .unwrap()
                .size_of()
                .unwrap()
                .const_cast(bin.context.i32_type(), false);
            let size = bin.builder.build_int_mul(elem_size, new_len, "");
            let size = bin.builder.build_int_add(size, vec_size, "");

            // Get the pointer to the last element and return it
            let slot_ptr = unsafe {
                bin.builder.build_gep(
                    a,
                    &[
                        bin.context.i32_type().const_zero(),
                        bin.context.i32_type().const_int(2, false),
                        bin.builder.build_int_mul(new_len, elem_size, ""),
                    ],
                    "data",
                )
            };
            let slot_ptr = bin.builder.build_pointer_cast(
                slot_ptr,
                llvm_elem_ty.ptr_type(AddressSpace::Generic),
                "slot_ptr",
            );
            if elem_ty.is_fixed_reference_type() {
                w.vars.get_mut(res).unwrap().value = slot_ptr.into();
            } else {
                let ret_val = bin.builder.build_load(slot_ptr, "");
                w.vars.get_mut(res).unwrap().value = ret_val;
            }

            // Reallocate and reassign the array pointer
            let a = bin.builder.build_pointer_cast(
                a,
                bin.context.i8_type().ptr_type(AddressSpace::Generic),
                "a",
            );

            let realloc_size = if ns.target == Target::Solana {
                bin.builder
                    .build_int_z_extend(size, bin.context.i64_type(), "")
            } else {
                size
            };

            let new = bin
                .builder
                .build_call(
                    bin.module.get_function("__realloc").unwrap(),
                    &[a.into(), realloc_size.into()],
                    "",
                )
                .try_as_basic_value()
                .left()
                .unwrap()
                .into_pointer_value();
            let dest = bin.builder.build_pointer_cast(
                new,
                llvm_ty.ptr_type(AddressSpace::Generic),
                "dest",
            );
            w.vars.get_mut(array).unwrap().value = dest.into();

            // Update the len and size field of the vector struct
            let len_ptr = unsafe {
                bin.builder.build_gep(
                    dest,
                    &[
                        bin.context.i32_type().const_zero(),
                        bin.context.i32_type().const_zero(),
                    ],
                    "len",
                )
            };
            let len_field = bin.builder.build_pointer_cast(
                len_ptr,
                bin.context.i32_type().ptr_type(AddressSpace::Generic),
                "len field",
            );
            bin.builder.build_store(len_field, new_len);

            let size_ptr = unsafe {
                bin.builder.build_gep(
                    dest,
                    &[
                        bin.context.i32_type().const_zero(),
                        bin.context.i32_type().const_int(1, false),
                    ],
                    "size",
                )
            };
            let size_field = bin.builder.build_pointer_cast(
                size_ptr,
                bin.context.i32_type().ptr_type(AddressSpace::Generic),
                "size field",
            );
            bin.builder.build_store(size_field, new_len);
        }
        Instr::AssertFailure { expr: None } => {
            target.assert_failure(
                bin,
                bin.context
                    .i8_type()
                    .ptr_type(AddressSpace::Generic)
                    .const_null(),
                bin.context.i32_type().const_zero(),
            );
        }
        Instr::AssertFailure { expr: Some(expr) } => {
            let v = expression(target, bin, expr, &w.vars, function, ns);

            let selector = 0x08c3_79a0u32;

            let (data, len) = target.abi_encode(
                bin,
                Some(bin.context.i32_type().const_int(selector as u64, false)),
                false,
                function,
                &[v],
                &[Type::String],
                ns,
            );

            target.assert_failure(bin, data, len);
        }
        Instr::Print { expr } => {
            let expr = expression(target, bin, expr, &w.vars, function, ns);

            target.print(bin, bin.vector_bytes(expr), bin.vector_len(expr));
        }
        Instr::Call {
            res,
            call: InternalCallTy::Static { cfg_no },
            args,
            ..
        } => {
            let f = &contract.cfg[*cfg_no];

            let mut parms = args
                .iter()
                .map(|p| expression(target, bin, p, &w.vars, function, ns).into())
                .collect::<Vec<BasicMetadataValueEnum>>();

            if !res.is_empty() {
                for v in f.returns.iter() {
                    parms.push(if ns.target == Target::Solana {
                        bin.build_alloca(function, bin.llvm_var_ty(&v.ty, ns), v.name_as_str())
                            .into()
                    } else {
                        bin.builder
                            .build_alloca(bin.llvm_var_ty(&v.ty, ns), v.name_as_str())
                            .into()
                    });
                }
            }

            if let Some(parameters) = bin.parameters {
                parms.push(parameters.into());
            }

            let ret = bin
                .builder
                .build_call(bin.functions[cfg_no], &parms, "")
                .try_as_basic_value()
                .left()
                .unwrap();

            let success = bin.builder.build_int_compare(
                IntPredicate::EQ,
                ret.into_int_value(),
                bin.return_values[&ReturnCode::Success],
                "success",
            );

            let success_block = bin.context.append_basic_block(function, "success");
            let bail_block = bin.context.append_basic_block(function, "bail");
            bin.builder
                .build_conditional_branch(success, success_block, bail_block);

            bin.builder.position_at_end(bail_block);

            bin.builder.build_return(Some(&ret));
            bin.builder.position_at_end(success_block);

            if !res.is_empty() {
                for (i, v) in f.returns.iter().enumerate() {
                    let val = bin
                        .builder
                        .build_load(parms[args.len() + i].into_pointer_value(), v.name_as_str());

                    let dest = w.vars[&res[i]].value;

                    if dest.is_pointer_value()
                        && !(v.ty.is_reference_type(ns)
                            || matches!(v.ty, Type::ExternalFunction { .. }))
                    {
                        bin.builder.build_store(dest.into_pointer_value(), val);
                    } else {
                        w.vars.get_mut(&res[i]).unwrap().value = val;
                    }
                }
            }
        }
        Instr::Call {
            res,
            call: InternalCallTy::Builtin { ast_func_no },
            args,
            ..
        } => {
            let mut parms = args
                .iter()
                .map(|p| expression(target, bin, p, &w.vars, function, ns).into())
                .collect::<Vec<BasicMetadataValueEnum>>();

            let callee = &ns.functions[*ast_func_no];

            if !res.is_empty() {
                for v in callee.returns.iter() {
                    parms.push(if ns.target == Target::Solana {
                        bin.build_alloca(function, bin.llvm_var_ty(&v.ty, ns), v.name_as_str())
                            .into()
                    } else {
                        bin.builder
                            .build_alloca(bin.llvm_var_ty(&v.ty, ns), v.name_as_str())
                            .into()
                    });
                }
            }

            let ret = target.builtin_function(bin, function, callee, &parms, ns);

            let success = bin.builder.build_int_compare(
                IntPredicate::EQ,
                ret.into_int_value(),
                bin.return_values[&ReturnCode::Success],
                "success",
            );

            let success_block = bin.context.append_basic_block(function, "success");
            let bail_block = bin.context.append_basic_block(function, "bail");
            bin.builder
                .build_conditional_branch(success, success_block, bail_block);

            bin.builder.position_at_end(bail_block);

            bin.builder.build_return(Some(&ret));
            bin.builder.position_at_end(success_block);

            if !res.is_empty() {
                for (i, v) in callee.returns.iter().enumerate() {
                    let val = bin
                        .builder
                        .build_load(parms[args.len() + i].into_pointer_value(), v.name_as_str());

                    let dest = w.vars[&res[i]].value;

                    if dest.is_pointer_value()
                        && !(v.ty.is_reference_type(ns)
                            || matches!(v.ty, Type::ExternalFunction { .. }))
                    {
                        bin.builder.build_store(dest.into_pointer_value(), val);
                    } else {
                        w.vars.get_mut(&res[i]).unwrap().value = val;
                    }
                }
            }
        }
        Instr::Call {
            res,
            call: InternalCallTy::Dynamic(call_expr),
            args,
            ..
        } => {
            let ty = call_expr.ty();

            let returns = if let Type::InternalFunction { returns, .. } = ty.deref_any() {
                returns
            } else {
                panic!("should be Type::InternalFunction type");
            };

            let mut parms = args
                .iter()
                .map(|p| expression(target, bin, p, &w.vars, function, ns).into())
                .collect::<Vec<BasicMetadataValueEnum>>();

            if !res.is_empty() {
                for ty in returns.iter() {
                    parms.push(
                        bin.build_alloca(function, bin.llvm_var_ty(ty, ns), "")
                            .into(),
                    );
                }
            }

            // on Solana, we need to pass the accounts parameter around
            if let Some(parameters) = bin.parameters {
                parms.push(parameters.into());
            }

            let callable = CallableValue::try_from(
                expression(target, bin, call_expr, &w.vars, function, ns).into_pointer_value(),
            )
            .unwrap();

            let ret = bin
                .builder
                .build_call(callable, &parms, "")
                .try_as_basic_value()
                .left()
                .unwrap();

            let success = bin.builder.build_int_compare(
                IntPredicate::EQ,
                ret.into_int_value(),
                bin.return_values[&ReturnCode::Success],
                "success",
            );

            let success_block = bin.context.append_basic_block(function, "success");
            let bail_block = bin.context.append_basic_block(function, "bail");
            bin.builder
                .build_conditional_branch(success, success_block, bail_block);

            bin.builder.position_at_end(bail_block);

            bin.builder.build_return(Some(&ret));
            bin.builder.position_at_end(success_block);

            if !res.is_empty() {
                for (i, ty) in returns.iter().enumerate() {
                    let val = bin
                        .builder
                        .build_load(parms[args.len() + i].into_pointer_value(), "");

                    let dest = w.vars[&res[i]].value;

                    if dest.is_pointer_value() && !ty.is_reference_type(ns) {
                        bin.builder.build_store(dest.into_pointer_value(), val);
                    } else {
                        w.vars.get_mut(&res[i]).unwrap().value = val;
                    }
                }
            }
        }
        Instr::Constructor {
            success,
            res,
            contract_no,
            encoded_args,
            encoded_args_len,
            value,
            gas,
            salt,
            space,
        } => {
            let encoded_args = expression(target, bin, encoded_args, &w.vars, function, ns);
            let encoded_args_len = expression(target, bin, encoded_args_len, &w.vars, function, ns);

            let address = bin.build_alloca(function, bin.address_type(ns), "address");

            let gas = expression(target, bin, gas, &w.vars, function, ns).into_int_value();
            let value = value
                .as_ref()
                .map(|v| expression(target, bin, v, &w.vars, function, ns).into_int_value());
            let salt = salt
                .as_ref()
                .map(|v| expression(target, bin, v, &w.vars, function, ns).into_int_value());
            let space = space
                .as_ref()
                .map(|v| expression(target, bin, v, &w.vars, function, ns).into_int_value());

            let success = match success {
                Some(n) => Some(&mut w.vars.get_mut(n).unwrap().value),
                None => None,
            };

            target.create_contract(
                bin,
                function,
                success,
                *contract_no,
                bin.builder.build_pointer_cast(
                    address,
                    bin.context.i8_type().ptr_type(AddressSpace::Generic),
                    "address",
                ),
                encoded_args,
                encoded_args_len,
                gas,
                value,
                salt,
                space,
                ns,
            );

            w.vars.get_mut(res).unwrap().value = bin.builder.build_load(address, "address");
        }
        Instr::ExternalCall {
            success,
            address,
            payload,
            value,
            gas,
            callty,
            accounts,
            seeds,
        } => {
            let gas = expression(target, bin, gas, &w.vars, function, ns).into_int_value();
            let value = expression(target, bin, value, &w.vars, function, ns).into_int_value();
            let payload_ty = payload.ty();
            let payload = expression(target, bin, payload, &w.vars, function, ns);

            let address = if let Some(address) = address {
                let address = expression(target, bin, address, &w.vars, function, ns);

                let addr = bin.build_array_alloca(
                    function,
                    bin.context.i8_type(),
                    bin.context
                        .i32_type()
                        .const_int(ns.address_length as u64, false),
                    "address",
                );

                bin.builder.build_store(
                    bin.builder.build_pointer_cast(
                        addr,
                        address.get_type().ptr_type(AddressSpace::Generic),
                        "address",
                    ),
                    address,
                );

                Some(addr)
            } else {
                None
            };

            let accounts = if let Some(accounts) = accounts {
                let ty = accounts.ty();

                let expr = expression(target, bin, accounts, &w.vars, function, ns);

                if let Some(n) = ty.array_length() {
                    let accounts = expr.into_pointer_value();
                    let len = bin.context.i32_type().const_int(n.to_u64().unwrap(), false);

                    Some((accounts, len))
                } else {
                    let addr = bin.vector_bytes(expr);
                    let len = bin.vector_len(expr);
                    Some((addr, len))
                }
            } else {
                None
            };

            let (payload_ptr, payload_len) = if payload_ty == Type::DynamicBytes {
                (bin.vector_bytes(payload), bin.vector_len(payload))
            } else {
                let ptr = payload.into_pointer_value();
                let len = ptr
                    .get_type()
                    .get_element_type()
                    .size_of()
                    .unwrap()
                    .const_cast(bin.context.i32_type(), false);

                (ptr, len)
            };

            let seeds = if let Some(seeds) = seeds {
                let len = seeds.ty().array_length().unwrap().to_u64().unwrap();
                let seeds_ty = bin.llvm_type(
                    &Type::Slice(Box::new(Type::Slice(Box::new(Type::Bytes(1))))),
                    ns,
                );

                let output_seeds = bin.build_array_alloca(
                    function,
                    seeds_ty,
                    bin.context.i64_type().const_int(len, false),
                    "seeds",
                );

                if let Expression::ArrayLiteral(_, _, _, exprs) = seeds {
                    for i in 0..len {
                        let val =
                            expression(target, bin, &exprs[i as usize], &w.vars, function, ns);

                        let seed_count = val
                            .get_type()
                            .into_pointer_type()
                            .get_element_type()
                            .into_array_type()
                            .len();

                        let dest = unsafe {
                            bin.builder.build_gep(
                                output_seeds,
                                &[
                                    bin.context.i32_type().const_int(i, false),
                                    bin.context.i32_type().const_zero(),
                                ],
                                "dest",
                            )
                        };

                        let val = bin.builder.build_pointer_cast(
                            val.into_pointer_value(),
                            dest.get_type().get_element_type().into_pointer_type(),
                            "seeds",
                        );

                        bin.builder.build_store(dest, val);

                        let dest = unsafe {
                            bin.builder.build_gep(
                                output_seeds,
                                &[
                                    bin.context.i32_type().const_int(i, false),
                                    bin.context.i32_type().const_int(1, false),
                                ],
                                "dest",
                            )
                        };

                        let val = bin.context.i64_type().const_int(seed_count as u64, false);

                        bin.builder.build_store(dest, val);
                    }
                }

                Some((output_seeds, bin.context.i64_type().const_int(len, false)))
            } else {
                None
            };

            let success = match success {
                Some(n) => Some(&mut w.vars.get_mut(n).unwrap().value),
                None => None,
            };

            target.external_call(
                bin,
                function,
                success,
                payload_ptr,
                payload_len,
                address,
                gas,
                value,
                accounts,
                seeds,
                callty.clone(),
                ns,
            );
        }
        Instr::ValueTransfer {
            success,
            address,
            value,
        } => {
            let value = expression(target, bin, value, &w.vars, function, ns).into_int_value();
            let address =
                expression(target, bin, address, &w.vars, function, ns).into_array_value();

            let addr = bin.build_alloca(function, bin.address_type(ns), "address");

            bin.builder.build_store(
                bin.builder.build_pointer_cast(
                    addr,
                    address.get_type().ptr_type(AddressSpace::Generic),
                    "address",
                ),
                address,
            );
            let success = match success {
                Some(n) => Some(&mut w.vars.get_mut(n).unwrap().value),
                None => None,
            };

            target.value_transfer(
                bin,
                function,
                success,
                bin.builder.build_pointer_cast(
                    addr,
                    bin.context.i8_type().ptr_type(AddressSpace::Generic),
                    "address",
                ),
                value,
                ns,
            );
        }
        Instr::AbiDecode {
            res,
            selector,
            exception_block: exception,
            tys,
            data,
            data_len,
        } => {
            let v = expression(target, bin, data, &w.vars, function, ns);

            let mut data = if data.ty().is_reference_type(ns) {
                bin.vector_bytes(v)
            } else {
                v.into_pointer_value()
            };

            let mut data_len = if let Some(len) = data_len {
                expression(target, bin, len, &w.vars, function, ns).into_int_value()
            } else {
                bin.vector_len(v)
            };

            if let Some(selector) = selector {
                let exception = exception.unwrap();

                let pos = bin.builder.get_insert_block().unwrap();

                blocks.entry(exception).or_insert({
                    work.push_back(Work {
                        block_no: exception,
                        vars: w.vars.clone(),
                    });

                    create_block(exception, bin, cfg, function, ns)
                });

                bin.builder.position_at_end(pos);

                let exception_block = blocks.get(&exception).unwrap();

                let has_selector = bin.builder.build_int_compare(
                    IntPredicate::UGT,
                    data_len,
                    bin.context.i32_type().const_int(4, false),
                    "has_selector",
                );

                let ok1 = bin.context.append_basic_block(function, "ok1");

                bin.builder
                    .build_conditional_branch(has_selector, ok1, exception_block.bb);
                bin.builder.position_at_end(ok1);

                let selector_data = bin
                    .builder
                    .build_load(
                        bin.builder.build_pointer_cast(
                            data,
                            bin.context.i32_type().ptr_type(AddressSpace::Generic),
                            "selector",
                        ),
                        "selector",
                    )
                    .into_int_value();

                let selector = if ns.target.is_substrate() {
                    *selector
                } else {
                    selector.to_be()
                };

                let correct_selector = bin.builder.build_int_compare(
                    IntPredicate::EQ,
                    selector_data,
                    bin.context.i32_type().const_int(selector as u64, false),
                    "correct_selector",
                );

                let ok2 = bin.context.append_basic_block(function, "ok2");

                bin.builder
                    .build_conditional_branch(correct_selector, ok2, exception_block.bb);

                bin.builder.position_at_end(ok2);

                data_len = bin.builder.build_int_sub(
                    data_len,
                    bin.context.i32_type().const_int(4, false),
                    "data_len",
                );

                data = unsafe {
                    bin.builder.build_gep(
                        bin.builder.build_pointer_cast(
                            data,
                            bin.context.i8_type().ptr_type(AddressSpace::Generic),
                            "data",
                        ),
                        &[bin.context.i32_type().const_int(4, false)],
                        "data",
                    )
                };
            }

            let mut returns = Vec::new();

            target.abi_decode(bin, function, &mut returns, data, data_len, tys, ns);

            for (i, ret) in returns.into_iter().enumerate() {
                w.vars.get_mut(&res[i]).unwrap().value = ret;
            }
        }
        Instr::Unreachable => {
            // Nothing to do; unreachable instruction should have already been inserteds
        }
        Instr::SelfDestruct { recipient } => {
            let recipient =
                expression(target, bin, recipient, &w.vars, function, ns).into_array_value();

            target.selfdestruct(bin, recipient, ns);
        }
        Instr::EmitEvent {
            event_no,
            data,
            data_tys,
            topics,
            topic_tys,
        } => {
            let data = data
                .iter()
                .map(|a| expression(target, bin, a, &w.vars, function, ns))
                .collect::<Vec<BasicValueEnum>>();

            let topics = topics
                .iter()
                .map(|a| expression(target, bin, a, &w.vars, function, ns))
                .collect::<Vec<BasicValueEnum>>();

            target.emit_event(
                bin, contract, function, *event_no, &data, data_tys, &topics, topic_tys, ns,
            );
        }
        Instr::WriteBuffer { buf, offset, value } => {
            let v = expression(target, bin, buf, &w.vars, function, ns);
            let data = bin.vector_bytes(v);

            let offset = expression(target, bin, offset, &w.vars, function, ns).into_int_value();
            let emit_value = expression(target, bin, value, &w.vars, function, ns);

            let start = unsafe { bin.builder.build_gep(data, &[offset], "start") };

            let is_bytes = if let Type::Bytes(n) = value.ty() {
                n
            } else {
                0
            };

            if is_bytes > 1 {
                let value_ptr = bin.build_alloca(
                    function,
                    emit_value.into_int_value().get_type(),
                    &format!("bytes{}", is_bytes),
                );
                bin.builder
                    .build_store(value_ptr, emit_value.into_int_value());
                bin.builder.build_call(
                    bin.module.get_function("__leNtobeN").unwrap(),
                    &[
                        bin.builder
                            .build_pointer_cast(
                                value_ptr,
                                bin.context.i8_type().ptr_type(AddressSpace::Generic),
                                "store",
                            )
                            .into(),
                        bin.builder
                            .build_pointer_cast(
                                start,
                                bin.context.i8_type().ptr_type(AddressSpace::Generic),
                                "dest",
                            )
                            .into(),
                        bin.context
                            .i32_type()
                            .const_int(is_bytes as u64, false)
                            .into(),
                    ],
                    "",
                );
            } else {
                let start = bin.builder.build_pointer_cast(
                    start,
                    emit_value.get_type().ptr_type(AddressSpace::Generic),
                    "start",
                );

                bin.builder.build_store(start, emit_value);
            }
        }
        Instr::MemCopy {
            source: from,
            destination: to,
            bytes,
        } => {
            let src = if from.ty().is_dynamic_memory() {
                bin.vector_bytes(expression(target, bin, from, &w.vars, function, ns))
            } else {
                expression(target, bin, from, &w.vars, function, ns).into_pointer_value()
            };

            let dest = if to.ty().is_dynamic_memory() {
                bin.vector_bytes(expression(target, bin, to, &w.vars, function, ns))
            } else {
                expression(target, bin, to, &w.vars, function, ns).into_pointer_value()
            };

            let size = expression(target, bin, bytes, &w.vars, function, ns);

            if matches!(bytes, Expression::NumberLiteral(..)) {
                let _ = bin
                    .builder
                    .build_memcpy(dest, 1, src, 1, size.into_int_value());
            } else {
                bin.builder.build_call(
                    bin.module.get_function("__memcpy").unwrap(),
                    &[dest.into(), src.into(), size.into()],
                    "",
                );
            }
        }
        Instr::Switch {
            cond,
            cases,
            default,
        } => {
            let pos = bin.builder.get_insert_block().unwrap();
            let cond = expression(target, bin, cond, &w.vars, function, ns);
            let cases = cases
                .iter()
                .map(|(exp, block_no)| {
                    let exp = expression(target, bin, exp, &w.vars, function, ns);
                    let bb = add_or_retrieve_block(
                        *block_no, pos, bin, function, blocks, work, w, cfg, ns,
                    );
                    (exp.into_int_value(), bb)
                })
                .collect::<Vec<(IntValue, inkwell::basic_block::BasicBlock)>>();

            let default_bb =
                add_or_retrieve_block(*default, pos, bin, function, blocks, work, w, cfg, ns);
            bin.builder.position_at_end(pos);
            bin.builder
                .build_switch(cond.into_int_value(), default_bb, cases.as_ref());
        }

        Instr::ReturnData { data, data_len } => {
            let data = if data.ty().is_reference_type(ns) {
                bin.vector_bytes(expression(target, bin, data, &w.vars, function, ns))
            } else {
                expression(target, bin, data, &w.vars, function, ns).into_pointer_value()
            };

            let data_len = expression(target, bin, data_len, &w.vars, function, ns);
            target.return_abi_data(bin, data, data_len);
        }

        Instr::ReturnCode { code } => {
            target.return_code(bin, bin.return_values[code]);
        }
    }
}

/// Add or retrieve a basic block from the blocks' hashmap
fn add_or_retrieve_block<'a>(
    block_no: usize,
    pos: inkwell::basic_block::BasicBlock<'a>,
    bin: &Binary<'a>,
    function: FunctionValue,
    blocks: &mut HashMap<usize, BasicBlock<'a>>,
    work: &mut VecDeque<Work<'a>>,
    w: &mut Work<'a>,
    cfg: &ControlFlowGraph,
    ns: &Namespace,
) -> inkwell::basic_block::BasicBlock<'a> {
    if let std::collections::hash_map::Entry::Vacant(e) = blocks.entry(block_no) {
        e.insert(create_block(block_no, bin, cfg, function, ns));
        work.push_back(Work {
            block_no,
            vars: w.vars.clone(),
        });
    }

    let bb = blocks.get(&block_no).unwrap();

    for (v, phi) in bb.phis.iter() {
        phi.add_incoming(&[(&w.vars[v].value, pos)]);
    }

    bb.bb
}
