interface Strategy {
  String run(String s);
}

interface Retryable extends Strategy {
}

abstract class BaseIter implements java.util.Iterator<String> {
  abstract int separatorStart(int start);
}

public class Splitter extends BaseIter implements Strategy, Retryable {
  public String run(String s) {
    return s;
  }
}
