package com.example.method;

/**
 * Enum methods are extracted like class methods; an enum member seed must keep
 * the enum constants and the intra-enum call path.
 */
public enum Level {

    LOW,
    HIGH;

    public int weight() {
        return 1;
    }

    public int scaled() {
        return weight() * 2;
    }
}
