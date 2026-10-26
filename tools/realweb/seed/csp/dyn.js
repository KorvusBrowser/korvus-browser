(function(){var L=window.L=window.L||[];L.push('dyn.js');
var s=document.createElement('script');s.innerText="(window.L=window.L||[]).push('C:inserted-innerText')";document.head.appendChild(s);
var t=document.createElement('script');t.nonce='abc';t.text="(window.L=window.L||[]).push('D:inserted-nonce')";document.head.appendChild(t);
var u=document.createElement('script');u.src='http://localhost:18301/x2.js';u.onerror=function(){L.push('G:error-event')};u.onload=function(){L.push('G:load-event')};document.head.appendChild(u);
})();
