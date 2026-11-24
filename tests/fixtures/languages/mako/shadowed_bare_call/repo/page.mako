<%def name="emit()">OUTER</%def>
<%def name="parameter(emit)">${emit()} ${local.emit()}</%def>
<%def name="assignment()">
<% emit = lambda: 'INNER' %>
${emit()} ${local.emit()}</%def>
<%def name="nested()"><%def name="emit()">INNER</%def>
${emit()} ${local.emit()}</%def>
