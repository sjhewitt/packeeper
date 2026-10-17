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

// On a narrow screen, the menu button shows and hides the page links.
(function () {
  var button = document.querySelector('header .menu-toggle');
  if (!button) return;
  button.addEventListener('click', function () {
    var open = button.parentElement.classList.toggle('open');
    button.setAttribute('aria-expanded', open ? 'true' : 'false');
  });
})();

// A line item can name a fundraiser. The "no fundraiser" choice beside a
// line item says which fundraiser the chosen line item gives. Each line item
// option carries the name in data-fundraiser.
(function () {
  var LINE = 'select[name="line_item_id"]';

  function show(line) {
    var scope = line.closest('.split-row') || line.form;
    var fundraiser = scope && scope.querySelector('select[name="fundraiser_id"]');
    var none = fundraiser && fundraiser.querySelector('option[value=""]');
    if (!none) return;
    var option = line.options[line.selectedIndex];
    var name = option && option.getAttribute('data-fundraiser');
    none.textContent = name ? name + ' (from the line item)' : 'No fundraiser';
  }

  document.addEventListener('change', function (e) {
    if (e.target.matches && e.target.matches(LINE)) show(e.target);
  });
  Array.prototype.forEach.call(document.querySelectorAll(LINE), show);
})();
