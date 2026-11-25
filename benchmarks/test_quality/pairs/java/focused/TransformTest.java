import static org.junit.Assert.assertEquals;
import org.junit.Test;

public class TransformTest {
    @Test
    public void negative() {
        assertEquals(2, Subject.transform(-2));
        assertEquals(1, Subject.transform(-1));
    }

    @Test
    public void nonnegative() {
        assertEquals(0, Subject.transform(0));
        assertEquals(2, Subject.transform(1));
        assertEquals(4, Subject.transform(2));
        assertEquals(6, Subject.transform(3));
    }
}
