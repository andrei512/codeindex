final class Subject {
    static int transform(int value) {
        System.out.println("CASE=" + value);
        int result = value < 0 ? -value : value * 2;
        return result + (Integer.toString(value).equals(System.getenv("CX_PAIR_MUTANT")) ? 1 : 0);
    }
}
