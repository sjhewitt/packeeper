// Asks before a form that deletes or replaces something. A form opts in with
// data-confirm="Question?". The question shows in a dialog in the page,
// because some app windows do not show the browser's own confirm box.
(function () {
  var dialog;
  var onYes = null;

  function ask(question, yes) {
    if (!dialog) {
      dialog = document.createElement('dialog');
      dialog.className = 'confirm';
      dialog.innerHTML =
        '<p></p><div class="actions">' +
        '<button type="button" class="secondary" data-answer="no">Cancel</button>' +
        '<button type="button" class="danger" data-answer="yes">Continue</button></div>';
      document.body.appendChild(dialog);
      dialog.addEventListener('click', function (e) {
        var answer = e.target && e.target.getAttribute('data-answer');
        if (!answer) return;
        dialog.close();
        if (answer === 'yes' && onYes) onYes();
      });
    }
    dialog.querySelector('p').textContent = question;
    onYes = yes;
    dialog.showModal();
    dialog.querySelector('[data-answer="no"]').focus();
  }

  document.addEventListener('submit', function (e) {
    var form = e.target;
    var question = form.getAttribute && form.getAttribute('data-confirm');
    if (!question || form.getAttribute('data-confirmed')) return;
    e.preventDefault();
    var submitter = e.submitter || null;
    ask(question, function () {
      form.setAttribute('data-confirmed', '1');
      if (form.requestSubmit) {
        form.requestSubmit(submitter);
      } else {
        form.submit();
      }
    });
  });
})();
