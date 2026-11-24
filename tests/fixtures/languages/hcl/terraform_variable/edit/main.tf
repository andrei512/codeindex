variable "region" {}
variable "zone" {}
output "region" {
  value = var.zone
}
output "description" {
  value = "var.region"
}
