let
  value = 1;
  outer = value;
in let
  value = 2;
in [ outer value ]
