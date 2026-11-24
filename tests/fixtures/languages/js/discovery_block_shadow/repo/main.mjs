function pick() { return "outer"; }
{
  const pick = () => "inner";
  console.log(pick());
}
console.log(pick());
