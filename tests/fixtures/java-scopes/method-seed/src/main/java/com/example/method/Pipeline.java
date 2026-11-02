package com.example.method;

/**
 * Method-seed fixture: intra-class calls and overloads. Overloads resolve by
 * name, so all three `prepare` methods and all three call sites stay together.
 */
public class Pipeline {

    public int run() {
        int first = prepare();
        int second = prepare(1);
        int third = prepare(2, 3);
        return toResult(first + second + third);
    }

    private int prepare() {
        return 0;
    }

    private int prepare(int value) {
        return value;
    }

    private int prepare(int left, int right) {
        return left + right;
    }

    private int toResult(int value) {
        return value;
    }

    private int skipped() {
        return 0;
    }
}
