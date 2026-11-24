variable "zone" { default = "east" }
locals {
  text = <<-END
    variable "ghost" {}
    real: ${var.zone}; literal: $${var.zone}
  END
}
