use strict;
sub visible { return 1 }
visible();
__DATA__
sub phantom { visible(); }
