#![cfg_attr(not(feature = "std"), no_std)]

use ink_lang as ink;
extern crate alloc;

#[ink::contract]
pub mod freezer {
    use bech32;
    use alloc::{vec::Vec, string::String};
    use ink_storage::{
        traits::{SpreadLayout, PackedLayout}
    };
    use scale::{
        Decode,
        Encode,
    };
    use ink_env::call::{*};

    #[cfg(feature = "std")]
    use scale_info::TypeInfo;

    /// Contract Storage
    /// Stores a list of validators
    #[ink(storage)]
    pub struct Freezer {
        validators: ink_storage::collections::HashMap<AccountId, ()>, // O(1) contains
        // action_id: pop_info
        pop_action: ink_storage::collections::HashMap<String, ActionInfo>,
        last_action: u128,
        wrapper: AccountId
    }

    /// Transfer to elrond chain event
    /// validators must subscribe to this
    #[ink(event)]
    pub struct Transfer {
        action_id: u128,
        to: String,
        value: Balance
    }

    #[ink(event)]
    pub struct ScCall {
        action_id: u128,
        to: String,
        endpoint: String,
        args: Vec<Vec<u8>> // TODO: Multiple Args
    }

    #[ink(event)]
    pub struct UnfreezeWrap {
        action_id: u128,
        to: String,
        value: Balance
    }

    #[derive(Clone, Debug, PartialEq, Encode, Decode, SpreadLayout, PackedLayout)]
    #[cfg_attr(feature = "std", derive(TypeInfo))]
    pub enum Action {
        Unfreeze {
            to: AccountId,
            value: Balance
        },
        RpcCall {
            to: AccountId,
            value: Balance,
            endpoint: [u8; 4],
            args: Option<u32>
        },
        SendWrapped {
            to: AccountId,
            value: Balance
        }
    }


    #[derive(Clone, Debug, PartialEq, Encode, Decode, SpreadLayout, PackedLayout)]
    #[cfg_attr(feature = "std", derive(TypeInfo))]
    pub struct ActionInfo {
        action: Action,
        validators: u32, // TODO: Use HSet
    }

    impl ActionInfo {
        fn new(action: Action) -> Self {
            Self {
                action,
                validators: 0,
            }
        }
    }

    impl Eq for ActionInfo {}

    // Hack
    // we don't really need this
    impl Default for ActionInfo {
        fn default() -> Self {
            unimplemented!()
        }
    }

    impl Freezer {
        #[ink(constructor)]
        pub fn new(erc20_addr: AccountId) -> Self {
            Self { 
                validators: Default::default(),
                pop_action: Default::default(),
                last_action: 0,
                wrapper: erc20_addr
            }
        }

        /// Emit a transfer event while locking
        /// existing coins
        #[ink(message)]
        #[ink(payable)]
        pub fn send(&mut self, to: String) {
            bech32::decode(&to).expect("Invalid address!");
            let val = self.env().transferred_balance();
            if val == 0 {
                panic!("Value must be > 0!")
            }
            self.last_action += 1;
            self.env().emit_event( Transfer {
                action_id: self.last_action,
                to,
                value: val,
            } )
        }

        fn erc20_burn(&self, acc: AccountId, value: Balance) {
            self.env().invoke_contract(
                &build_call()
                    .callee(self.wrapper)
                    .transferred_value(0)
                    .exec_input(
                        ExecutionInput::new(Selector::new([0xB0, 0xF9, 0xC0, 0x19]))
                            .push_arg(value)
                            .push_arg(acc)
                    )
                    .returns::<()>()
                    .params()
            ).expect("Failed to burn coins!");
        }

        fn erc20_mint(&self, acc: AccountId, value: Balance) {
            self.env().invoke_contract(
                &build_call()
                    .callee(self.wrapper)
                    .transferred_value(0)
                    .exec_input(
                        ExecutionInput::new(Selector::new([0x31, 0x91, 0xC0, 0x19]))
                            .push_arg(value)
                            .push_arg(acc)
                    )
                    .returns::<()>()
                    .params()
            ).expect("Failed to mint coins!");
        }

        /// Burn erc20 token & emit event
        #[ink(message)]
        pub fn withdraw_wrapper(&mut self, to: String, value: Balance) {
            bech32::decode(&to).expect("Invalid address!");
            if value <= 0 {
                panic!("Value must be > 0!");
            }

            let caller = self.env().caller();

            self.erc20_burn(caller, value);
            self.last_action += 1;
            self.env().emit_event( UnfreezeWrap {
                action_id: self.last_action,
                to,
                value
            } )
        }

        /// Emit an SCCall event
        /// TODO: Charge some token amount for this
        #[ink(message)]
        pub fn send_sc_call(&mut self, target_contract: String, endpoint: String, args: Vec<Vec<u8>>) {
            bech32::decode(&target_contract).expect("Invalid address!");
            self.last_action += 1;
            self.env().emit_event( ScCall {
                action_id: self.last_action,
                to: target_contract,
                endpoint,
                args
            } )
        }

        fn exec_action(&mut self, action: Action) {
            match action {
                Action::Unfreeze { to, value } => self.env().transfer(to, value).unwrap(),
                Action::RpcCall { to, value, endpoint, args } => {
                    if let Some(arg) = args {
                        let exargs = ExecutionInput::new(Selector::new(endpoint))
                            .push_arg(arg.clone()); // TODO: Support multiple args

                        self.env().invoke_contract(
                            &build_call()
                                .callee(to)
                                .transferred_value(value)
                                .exec_input(exargs)
                                .returns::<()>()
                                .params()
                        ).unwrap();
                    } else {
                        self.env().invoke_contract(
                            &build_call()
                                .callee(to)
                                .transferred_value(value)
                                .exec_input(ExecutionInput::new(Selector::new(endpoint)))
                                .returns::<()>()
                                .params()
                        ).unwrap();
                    }
                },
                Action::SendWrapped { to, value } => {
                    self.erc20_mint(to, value);
                }
            }
        }

        fn verify_action(&mut self, action_id: String, action: Action) {
            let caller = self.env().caller();
            if self.validators.get(&caller).is_none() {
                panic!("not a validator!")
            }
            let validator_cnt = self.validator_cnt();

            let ref mut info = self.pop_action.entry(action_id.clone())
                .or_insert_with(|| ActionInfo::new(action));
            info.validators += 1;
            let act = info.action.clone();
            let validated = info.validators;
            core::mem::drop(info);

            if validated == (2*validator_cnt/3)+1 {
                self.exec_action(act);
            }

            if validated == validator_cnt {
                self.pop_action.take(&action_id).unwrap();
            }
        }

        /// unfreeze tokens and send them to an address
        /// only validators can call this
        #[ink(message)]
        pub fn pop(&mut self, action_id: String, to: AccountId, value: Balance) {
            self.verify_action(action_id, Action::Unfreeze { to, value })
        }

        #[ink(message)]
        pub fn sc_call_verify(&mut self, action_id: String, to: AccountId, value: Balance, endpoint: [u8; 4], args: Option<u32>) {
            self.verify_action(action_id, Action::RpcCall { to, value, endpoint, args })
        }

        #[ink(message)]
        pub fn send_wrapper_verify(&mut self, action_id: String, to: AccountId, value: Balance) {
            self.verify_action(action_id, Action::SendWrapped { to, value });
        }

        /// Subscribe to events & become a validator
        /// Placeholder for now
        /// TODO: Proper implementation
        #[ink(message)]
        pub fn subscribe(&mut self) {
            self.validators.insert(self.env().caller(), ());
        }

        /// Number of validators
        /// only for debugging
        #[ink(message)]
        pub fn validator_cnt(&self) -> u32 {
            self.validators.len()
        }

        fn pop_cnt(&self) -> u32 {
            self.pop_action.len()
        }
    }

    /// Unit tests in Rust are normally defined within such a `#[cfg(test)]`
    /// module and test functions are marked with a `#[test]` attribute.
    /// The below code is technically just normal Rust code.
    #[cfg(test)]
    mod tests {
        /// Imports all the definitions from the outer scope so we can use them here.
        use super::*;

        /// Imports `ink_lang` so we can use `#[ink::test]`.
        use ink_lang as ink;

        /// Check default impl 
        #[ink::test]
        fn default_works() {
            let freezer = Freezer::default();
            assert_eq!(freezer.validator_cnt(), 0);
        }

        /// Check if validators can be added
        #[ink::test]
        fn subscribe_test() {
            let mut freezer = Freezer::default();
            freezer.subscribe();
            assert_eq!(freezer.validator_cnt(), 1);
        }

        #[ink::test]
        fn send_test() {
            let mut freezer = Freezer::default();
            freezer.send("erd1qyu5wthldzr8wx5c9ucg8kjagg0jfs53s8nr3zpz3hypefsdd8ssycr6th".to_string());
            let evs = ink_env::test::recorded_events().collect::<Vec<_>>();
            assert_eq!(evs.len(), 1);
        }

        /// Check if validators can pop transactions properly
        #[ink::test]
        fn pop() {
            let mut freezer = Freezer::default();

            let acc: AccountId = ink_env::test::default_accounts::<ink_env::DefaultEnvironment>().unwrap().alice;
            let action = "0".to_string();

            //assert!(!freezer.pop(action.clone(), acc.clone().into(), 0x0));

            freezer.subscribe();
            freezer.pop(action.clone(), acc.clone().into(), 0x0);
            assert_eq!(freezer.pop_cnt(), 0)
        }
    }
}
