// Copyright 2017 The Exonum Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

extern crate bodyparser;
extern crate iron;
extern crate router;
extern crate serde;
extern crate serde_json;

use exonum::blockchain::{ApiContext, Blockchain, Service, Transaction, TransactionService, ObserverService};
use exonum::node::ApiSender;
use exonum::messages::{Message, RawTransaction};
use exonum::storage::{Fork, MapIndex, Snapshot};
use exonum::crypto::{Hash, PublicKey};
use exonum::encoding;
use exonum::encoding::serialize::FromHex;
use exonum::api::{Api, ApiError};
use self::iron::prelude::*;
use self::iron::headers::ContentType;
use self::iron::{Handler, IronError};
use self::iron::status::Status;
use self::router::Router;

// // // // // // // // // // CONSTANTS // // // // // // // // // //

const SERVICE_ID: u16 = 1;
const TX_CREATE_WALLET_ID: u16 = 1;
const TX_TRANSFER_ID: u16 = 2;

/// Initial balance of newly created wallet.
pub const INIT_BALANCE: u64 = 100;

// // // // // // // // // // PERSISTENT DATA // // // // // // // // // //

encoding_struct! {
    struct Wallet {
        pub_key: &PublicKey,
        name: &str,
        balance: u64,
    }
}

impl Wallet {
    pub fn increase(self, amount: u64) -> Self {
        let balance = self.balance() + amount;
        Self::new(self.pub_key(), self.name(), balance)
    }

    pub fn decrease(self, amount: u64) -> Self {
        let balance = self.balance() - amount;
        Self::new(self.pub_key(), self.name(), balance)
    }
}

// // // // // // // // // // DATA LAYOUT // // // // // // // // // //

pub struct CurrencySchema<S> {
    view: S,
}

impl<S: AsRef<Snapshot>> CurrencySchema<S> {
    pub fn new(view: S) -> Self {
        CurrencySchema { view }
    }

    pub fn wallets(&self) -> MapIndex<&Snapshot, PublicKey, Wallet> {
        MapIndex::new("cryptocurrency.wallets", self.view.as_ref())
    }

    /// Get a separate wallet from the storage.
    pub fn wallet(&self, pub_key: &PublicKey) -> Option<Wallet> {
        self.wallets().get(pub_key)
    }
}

impl<'a> CurrencySchema<&'a mut Fork> {
    pub fn wallets_mut(&mut self) -> MapIndex<&mut Fork, PublicKey, Wallet> {
        MapIndex::new("cryptocurrency.wallets", self.view)
    }
}

// // // // // // // // // // TRANSACTIONS // // // // // // // // // //

/// Create a new wallet.
message! {
    struct TxCreateWallet {
        const TYPE = SERVICE_ID;
        const ID = TX_CREATE_WALLET_ID;

        pub_key: &PublicKey,
        name: &str,
    }
}

/// Transfer coins between the wallets.
message! {
    struct TxTransfer {
        const TYPE = SERVICE_ID;
        const ID = TX_TRANSFER_ID;

        from: &PublicKey,
        to: &PublicKey,
        amount: u64,
        seed: u64,
    }
}

// // // // // // // // // // CONTRACTS // // // // // // // // // //

impl Transaction for TxCreateWallet {
    /// Verify integrity of the transaction by checking the transaction
    /// signature.
    fn verify(&self) -> bool {
        self.verify_signature(self.pub_key())
    }

    /// Apply logic to the storage when executing the transaction.
    fn execute(&self, view: &mut Fork) {
        let mut schema = CurrencySchema { view };
        if schema.wallet(self.pub_key()).is_none() {
            let wallet = Wallet::new(self.pub_key(), self.name(), INIT_BALANCE);
            schema.wallets_mut().put(self.pub_key(), wallet)
        }
    }
}

impl Transaction for TxTransfer {
    /// Check if the sender is not the receiver. Check correctness of the
    /// sender's signature.
    fn verify(&self) -> bool {
        (*self.from() != *self.to()) && self.verify_signature(self.from())
    }

    /// Retrieve two wallets to apply the transfer. Check the sender's
    /// balance and apply changes to the balances of the wallets.
    fn execute(&self, view: &mut Fork) {
        let mut schema = CurrencySchema { view };
        let sender = schema.wallet(self.from());
        let receiver = schema.wallet(self.to());
        if let (Some(sender), Some(receiver)) = (sender, receiver) {
            let amount = self.amount();
            if sender.balance() >= amount {
                let sender = sender.decrease(amount);
                let receiver = receiver.increase(amount);
                let mut wallets = schema.wallets_mut();
                wallets.put(self.from(), sender);
                wallets.put(self.to(), receiver);
            }
        }
    }
}

// // // // // // // // // // REST API // // // // // // // // // //

#[derive(Clone)]
struct CryptocurrencyApi {
    channel: ApiSender,
    blockchain: Blockchain,
}

/// Shortcut to get data on wallets.
impl CryptocurrencyApi {
    fn wallet(&self, pub_key: &PublicKey) -> Option<Wallet> {
        let view = self.blockchain.snapshot();
        let schema = CurrencySchema::new(view);
        schema.wallet(pub_key)
    }

    fn wallets(&self) -> Vec<Wallet> {
        let view = self.blockchain.snapshot();
        let schema = CurrencySchema::new(view);
        let wallets = schema.wallets();
        let wallets = wallets.values();
        wallets.collect()
    }

    /// Endpoint for retrieving a single wallet.
    fn get_wallet(&self, req: &mut Request) -> IronResult<Response> {
        use self::iron::modifiers::Header;

        let path = req.url.path();
        let wallet_key = path.last().unwrap();
        let public_key = PublicKey::from_hex(wallet_key).map_err(|e| {
            IronError::new(ApiError::FromHex(e), (
                Status::BadRequest,
                Header(ContentType::json()),
                "\"Invalid request param: `pub_key`\"",
            ))
        })?;
        if let Some(wallet) = self.wallet(&public_key) {
            self.ok_response(&serde_json::to_value(wallet).unwrap())
        } else {
            Err(IronError::new(ApiError::NotFound, (
                Status::NotFound,
                Header(ContentType::json()),
                "\"Wallet not found\"",
            )))
        }
    }

    /// Endpoint for retrieving all wallets in the blockchain.
    fn get_wallets(&self, _: &mut Request) -> IronResult<Response> {
        self.ok_response(&serde_json::to_value(&self.wallets()).unwrap())
    }
}

impl Api for CryptocurrencyApi {
    fn wire(&self, router: &mut Router) {
        let self_ = self.clone();
        let self_ = self.clone();
        let get_wallets = move |req: &mut Request| self_.get_wallets(req);
        let self_ = self.clone();
        let get_wallet = move |req: &mut Request| self_.get_wallet(req);

        router.get("/v1/wallets", get_wallets, "get_wallets");
        router.get("/v1/wallet/:pub_key", get_wallet, "get_wallet");
    }
}

// // // // // // // // // // SERVICE DECLARATION // // // // // // // // // //

/// Define the service.
pub struct CurrencyService;

transaction_set! {
    CurrencyTransactions {
        TxTransfer, TxCreateWallet
    }
}

impl TransactionService for CurrencyService {
    const ID: u16 = SERVICE_ID;
    const NAME: &'static str = "cryptocurrency";
    type Transactions = CurrencyTransactions;

    fn state_hash(&self, snapshot: &Snapshot) -> Vec<Hash> {
        Vec::new()
    }

    /// Create a REST `Handler` to process web requests to the node.
    fn wire_public_api(&self, router: &mut Router, ctx: &ApiContext) {
        let api = CryptocurrencyApi {
            channel: ctx.node_channel().clone(),
            blockchain: ctx.blockchain().clone(),
        };
        api.wire(router);
    }
}

pub struct WalletsService;

impl ObserverService for WalletsService {
    const ID: u16 = 2;
    const NAME: &'static str = "wallets";

    fn wire_public_api(&self, router: &mut Router, ctx: &ApiContext) {
        #[derive(Clone)]
        struct WalletsApi {
            channel: ApiSender,
            blockchain: Blockchain,
        }

        impl WalletsApi {
            fn get_wallets(&self, _: &mut Request) -> IronResult<Response> {
                self.ok_response(&serde_json::to_value(&Vec::<Wallet>::new()).unwrap())
            }
        };

        impl Api for WalletsApi {
            fn wire(&self, router: &mut Router) {
                let self_ = self.clone();
                let get_wallets = move |req: &mut Request| self_.get_wallets(req);
                router.get("/v1/wallets", get_wallets, "get_wallets");
            }
        }

        let api = WalletsApi {
            channel: ctx.node_channel().clone(),
            blockchain: ctx.blockchain().clone(),
        };
        api.wire(router);
    }
}
