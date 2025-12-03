

use zcash_primitives::transaction::Transaction as ZcashTransaction;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde::de::Error;
use zcash_protocol::consensus::BranchId;

#[derive(Debug)]
pub struct Transaction(pub ZcashTransaction);

impl Serialize for Transaction {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut buffer = Vec::new();
        self.0.write(&mut buffer).map_err(serde::ser::Error::custom)?;
        let hex_string = hex::encode(buffer);
        serializer.serialize_str(&hex_string)
    }
}

impl<'de> Deserialize<'de> for Transaction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let hex_string = String::deserialize(deserializer)?;
        let bytes = hex::decode(&hex_string).map_err(Error::custom)?;
        
        let branch_ids = [
            BranchId::Nu5,
            BranchId::Canopy,
            BranchId::Heartwood,
            BranchId::Blossom,
            BranchId::Sapling,
            BranchId::Overwinter,
            BranchId::Sprout,
        ];
        
        for branch_id in branch_ids.iter() {
            let mut reader = bytes.as_slice();
            if let Ok(tx) = ZcashTransaction::read(&mut reader, *branch_id) {
                return Ok(Transaction(tx));
            }
        }
        
        Err(Error::custom("Failed to read transaction with any branch ID"))
    }
}


