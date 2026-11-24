use strict;
use warnings;
package Worker;
sub emit {}
sub flush {}
sub run {
    Worker::emit();
}
package Other;
sub emit {}
1;
