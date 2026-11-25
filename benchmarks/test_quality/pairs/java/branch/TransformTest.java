import static org.junit.Assert.assertEquals;
import org.junit.Test;

public class TransformTest {
    @Test
    public void transform() {
        for (int value : new int[]{-2, -1, 0, 1, 2, 3}) {
            if (value < 0) {
                assertEquals(-value, Subject.transform(value));
            } else {
                assertEquals(value * 2, Subject.transform(value));
            }
        }
    }
}
