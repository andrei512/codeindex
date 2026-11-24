const value = 1;
function run({value: renamed}) {
  return renamed;
}
console.log(run({value: 42}), value);
