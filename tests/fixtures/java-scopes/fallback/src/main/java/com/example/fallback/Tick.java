package com.example.fallback;

/** Must never be bundled: the chained call into it cannot be typed. */
public class Tick {
    public int tick() {
        return 1;
    }
}
