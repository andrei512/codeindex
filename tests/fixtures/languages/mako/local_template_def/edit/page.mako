<%def name="render()">green</%def>
<%def name="format_page()">red</%def>
<%def name="wrapper()">
${local.format_page()}
render()
</%def>
