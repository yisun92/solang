// SPDX-License-Identifier: GPL-3.0
pragma solidity ^0.8.4;
contract SimpleAuction {
    bool ended;

    error AuctionAlreadyEnded();
    /// Bid on the auction with the value sent
    /// together with this transaction.
    /// The value will only be refunded if the
    /// auction is not won.
    function bid() external payable {
        if (!ended) {
            revert(AuctionAlreadyEnded());
        }
    }
}