package com.example.fallback;

/** Control file: nothing calls it, so it must never show up in a bundle. */
public class Dead {
    public int neverCalled() {
        return 0;
    }
}
