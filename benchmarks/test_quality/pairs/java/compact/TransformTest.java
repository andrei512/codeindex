import static org.junit.Assert.assertEquals;
import org.junit.Test;

public class TransformTest {
    @Test
    public void transform() {
        int[][] cases = {
                {-2, 2}, {-1, 1}, {0, 0}, {1, 2}, {2, 4}, {3, 6}
            };
        for (int[] c : cases) {
            assertEquals(c[1], Subject.transform(c[0]));
        }
    }
}
