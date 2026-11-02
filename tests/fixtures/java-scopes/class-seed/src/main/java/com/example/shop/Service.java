package com.example.shop;

import com.example.shop.base.BaseService;
import com.example.shop.util.Formats;

import static com.example.shop.util.Formats.money;

/**
 * Class-seed fixture. Every receiver kind of the plan appears exactly once:
 * field, parameter, local, static type, static import, inherited bare call and
 * inherited call through `super`.
 */
public class Service extends BaseService {
    private final Repo repo = new Repo();

    public int total(Ledger param) {
        Cart local = new Cart();
        int a = repo.repoSize();     // field
        int b = param.ledgerTotal(); // parameter
        int c = local.cartCount();   // local
        int d = Formats.zero();      // static type
        int e = base();              // inherited, bare
        return a + b + c + d + e;
    }

    public void inspect() {
        audit();      // inherited, bare
        super.base(); // inherited, super
    }

    public String label() {
        return money(7); // static import
    }
}
