package com.example.fallback;

/**
 * Fallback fixture. `unknown` and `chained` must stop at the first hop,
 * `missing` must fall back to the whole receiver class, and `constructed`
 * must resolve `new Type().method()`.
 */
public class Controller {

    private final Repo repo;

    public Controller(Repo repo) {
        this.repo = repo;
    }

    public int unknown(Bridge bridge) {
        // lookup() returns Object, so tick() has no known receiver type
        return bridge.lookup().tick();
    }

    public int missing() {
        // Repo is known but has no missing(): keep the whole class
        return repo.missing();
    }

    public int chained() {
        // the first hop resolves, the chained tick() does not
        return new Bridge().lookup().tick();
    }

    public int constructed() {
        // new Type().method() is a supported receiver form
        return new Repo().size();
    }
}
