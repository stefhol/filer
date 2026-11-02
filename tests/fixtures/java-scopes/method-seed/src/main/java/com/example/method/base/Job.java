package com.example.method.base;

/**
 * Abstract class seeded through a concrete method: the abstract callee has no
 * body, so it is kept as a signature while the concrete sibling stays whole.
 */
public abstract class Job {

    public int runOnce() {
        return step() + labels();
    }

    protected abstract int step();

    protected int labels() {
        return 0;
    }
}
